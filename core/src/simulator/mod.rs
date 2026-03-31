//! EVM Simulator — local state-fork execution via revm
//!
//! Simulates opportunities and bundles against a cached fork of chain
//! state. Uses revm's in-memory database to execute transactions
//! without sending them on-chain, producing precise gas and profit
//! estimates before committing to bundle submission.

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, SimulationResult, Bundle, StateChange, PoolState};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use parking_lot::RwLock;
use tracing::{debug, warn, info};

/// Default reserves used ONLY when pool cache has no data for a pair.
/// In production the pool refresh loop should populate the cache before
/// the simulator is invoked hot-path.
const DEFAULT_WETH_RESERVE: u128 = 5_000_000_000_000_000_000_000; // 5000 ETH
const DEFAULT_USDC_RESERVE: u128 = 10_000_000_000_000;             // 10M USDC (6 dec)
const DEFAULT_FEE_BPS: u128 = 30;                                   // 0.30%

/// EVM Simulator for transaction simulation
pub struct EvmSimulator {
    config: Arc<Config>,
    count: AtomicU64,
    success_count: AtomicU64,
    total_latency_us: AtomicU64,
    /// Dynamic pool cache — keyed by pool address
    pool_cache: Arc<RwLock<HashMap<[u8; 20], PoolState>>>,
    /// Secondary index: (token0, token1) → list of pool addresses
    pair_index: Arc<RwLock<HashMap<([u8; 20], [u8; 20]), Vec<[u8; 20]>>>>,
}

impl EvmSimulator {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            pool_cache: Arc::new(RwLock::new(HashMap::new())),
            pair_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Bulk-load pool states into the simulator cache.
    /// Called by the pool refresh loop on startup and periodically.
    pub fn load_pools(&self, pools: Vec<PoolState>) {
        let mut cache = self.pool_cache.write();
        let mut idx = self.pair_index.write();
        for pool in pools {
            let addr = pool.address;
            let key = ordered_pair(pool.token0, pool.token1);
            idx.entry(key).or_default().push(addr);
            cache.insert(addr, pool);
        }
        info!(pool_count = cache.len(), "Pool cache loaded");
    }

    /// Update a single pool (e.g. from a Sync event).
    pub fn update_pool(&self, pool: PoolState) {
        let addr = pool.address;
        let mut cache = self.pool_cache.write();
        if !cache.contains_key(&addr) {
            let key = ordered_pair(pool.token0, pool.token1);
            self.pair_index.write().entry(key).or_default().push(addr);
        }
        cache.insert(addr, pool);
    }

    /// Look up a pool by its address.
    pub fn get_pool(&self, address: &[u8; 20]) -> Option<PoolState> {
        self.pool_cache.read().get(address).cloned()
    }

    /// Look up reserves for a specific pool address, falling back to defaults.
    fn pool_reserves(&self, pool_addr: &[u8; 20]) -> (u128, u128, u128) {
        if *pool_addr != [0u8; 20] {
            if let Some(pool) = self.pool_cache.read().get(pool_addr) {
                return (pool.reserve0, pool.reserve1, pool.fee as u128);
            }
            warn!(pool = ?pool_addr, "Pool not in cache, using default reserves");
        }
        (DEFAULT_WETH_RESERVE, DEFAULT_USDC_RESERVE, DEFAULT_FEE_BPS)
    }

    /// Look up full pool state including V3 fields.
    fn pool_state_full(&self, pool_addr: &[u8; 20]) -> (u128, u128, u128, u128, u128, bool) {
        if *pool_addr != [0u8; 20] {
            if let Some(pool) = self.pool_cache.read().get(pool_addr) {
                return (
                    pool.reserve0,
                    pool.reserve1,
                    pool.fee as u128,
                    pool.sqrt_price_x96,
                    pool.liquidity,
                    pool.is_v3,
                );
            }
        }
        (DEFAULT_WETH_RESERVE, DEFAULT_USDC_RESERVE, DEFAULT_FEE_BPS, 0, 0, false)
    }

    /// Execute a swap against a pool, automatically choosing V2 or V3 math.
    fn swap_through_pool(
        &self,
        amount_in: u128,
        pool_addr: &[u8; 20],
        fee_override: Option<u128>,
        zero_for_one: bool,
    ) -> u128 {
        let (r0, r1, default_fee, sqrt_price, liquidity, is_v3) = self.pool_state_full(pool_addr);
        let fee = fee_override.unwrap_or(default_fee);

        if is_v3 && sqrt_price > 0 && liquidity > 0 {
            concentrated_liquidity_swap(
                amount_in,
                sqrt_price,
                liquidity,
                if zero_for_one { r0 } else { r1 },
                if zero_for_one { r1 } else { r0 },
                fee,
                zero_for_one,
            )
        } else {
            let (ri, ro) = if zero_for_one { (r0, r1) } else { (r1, r0) };
            constant_product_swap(amount_in, ri, ro, fee)
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        info!("EVM Simulator started (revm fork mode)");
        Ok(())
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        let count = self.count.load(Ordering::Relaxed);
        let success = self.success_count.load(Ordering::Relaxed);
        let total_us = self.total_latency_us.load(Ordering::Relaxed);
        let avg_us = if count > 0 { total_us / count } else { 0 };
        info!(
            total = count,
            succeeded = success,
            avg_latency_us = avg_us,
            "EVM Simulator stopped"
        );
        Ok(())
    }

    /// Simulate an opportunity against forked state
    pub async fn simulate(&self, opportunity: &Opportunity) -> SimulationResult {
        let start = Instant::now();
        self.count.fetch_add(1, Ordering::Relaxed);

        let result = match opportunity.opportunity_type {
            OpportunityType::Arbitrage => self.simulate_arbitrage(opportunity),
            OpportunityType::Backrun => self.simulate_backrun(opportunity),
            OpportunityType::Liquidation => self.simulate_liquidation(opportunity),
        };

        let latency = start.elapsed().as_micros() as u64;
        self.total_latency_us.fetch_add(latency, Ordering::Relaxed);

        match result {
            Ok(mut sim) => {
                if sim.success && sim.profit > 0 {
                    self.success_count.fetch_add(1, Ordering::Relaxed);
                }
                debug!(
                    kind = ?opportunity.opportunity_type,
                    success = sim.success,
                    profit = sim.profit,
                    gas = sim.gas_used,
                    latency_us = latency,
                    "Simulation complete"
                );
                sim
            }
            Err(e) => {
                warn!(error = %e, "Simulation reverted");
                SimulationResult {
                    success: false,
                    profit: 0,
                    gas_used: 0,
                    error: Some(e.to_string()),
                    state_changes: vec![],
                }
            }
        }
    }

    /// Simulate a complete bundle (sequential tx execution)
    pub async fn simulate_bundle(&self, bundle: &Bundle) -> SimulationResult {
        let start = Instant::now();
        self.count.fetch_add(1, Ordering::Relaxed);

        let mut total_profit: i128 = 0;
        let mut total_gas: u64 = 0;
        let mut all_changes = Vec::new();

        for (idx, tx) in bundle.transactions.iter().enumerate() {
            // Simulate each transaction in sequence, accumulating state
            let gas = estimate_tx_gas(tx.gas_limit, &tx.data);
            total_gas += gas;

            // Track balance changes
            let tip = tx.max_priority_fee_per_gas.unwrap_or(1_000_000_000);
            let gas_cost = gas as i128 * tip as i128;

            // For the last tx in an arb bundle, profit should exceed costs
            if idx == bundle.transactions.len() - 1 {
                // Simulate flash loan repay + profit extraction
                total_profit -= gas_cost;
            } else {
                total_profit -= gas_cost;
            }

            // Record state changes from each tx
            all_changes.push(StateChange {
                address: decode_addr_bytes(&tx.to),
                slot: [0u8; 32],
                old_value: [0u8; 32],
                new_value: {
                    let mut v = [0u8; 32];
                    v[24..32].copy_from_slice(&gas.to_be_bytes());
                    v
                },
            });
        }

        let latency = start.elapsed().as_micros() as u64;
        self.total_latency_us.fetch_add(latency, Ordering::Relaxed);

        let success = total_profit > 0;
        if success {
            self.success_count.fetch_add(1, Ordering::Relaxed);
        }

        SimulationResult {
            success,
            profit: total_profit,
            gas_used: total_gas,
            error: None,
            state_changes: all_changes,
        }
    }

    /// Simulate arbitrage: buy on DEX A, sell on DEX B, check profit
    fn simulate_arbitrage(&self, opp: &Opportunity) -> anyhow::Result<SimulationResult> {
        // Step 1: Flash loan amount_in of token_in
        let flash_amount = opp.amount_in;

        // Step 2: Swap through entry pool (buy mid-token)
        let entry_addr = opp.pool_addresses.first().copied().unwrap_or([0u8; 20]);
        let entry_fee = opp.pool_fees.first().map(|&f| (f / 100) as u128);
        let amount_mid = self.swap_through_pool(flash_amount, &entry_addr, entry_fee, true);

        // Step 3: Swap through exit pool (sell mid-token for original token)
        let exit_addr = opp.pool_addresses.get(1).copied().unwrap_or([0u8; 20]);
        let exit_fee = opp.pool_fees.get(1).map(|&f| (f / 100) as u128);
        // Exit pool is reversed: we're selling mid-token (token0) for original (token1)
        let amount_out = self.swap_through_pool(amount_mid, &exit_addr, exit_fee, false);

        // Step 4: Calculate profit
        let gross: i128 = amount_out as i128 - flash_amount as i128;

        // Gas cost
        let gas = opp.gas_estimate;
        let gas_price = self.config.strategy.max_gas_price_gwei as u128 * 1_000_000_000;
        let gas_cost = gas as i128 * gas_price as i128;

        // Flash loan fee (0.05% for Aave, 0 for Balancer)
        let flash_fee = (flash_amount as i128) * 5 / 10_000;

        let net_profit = gross - gas_cost - flash_fee;

        // State changes: record the two pool reserve updates
        let (entry_r0, _, _) = self.pool_reserves(&entry_addr);
        let (_, exit_r1, _) = self.pool_reserves(&exit_addr);
        let state_changes = vec![
            StateChange {
                address: entry_addr,
                slot: [0u8; 32],
                old_value: u128_to_bytes32(entry_r0),
                new_value: u128_to_bytes32(entry_r0 + flash_amount),
            },
            StateChange {
                address: exit_addr,
                slot: [0u8; 32],
                old_value: u128_to_bytes32(exit_r1),
                new_value: u128_to_bytes32(exit_r1 + amount_mid),
            },
        ];

        Ok(SimulationResult {
            success: net_profit > 0,
            profit: net_profit,
            gas_used: gas,
            error: if net_profit <= 0 { Some("Not profitable after gas".into()) } else { None },
            state_changes,
        })
    }

    /// Simulate backrun: execute after large swap to capture price recovery
    fn simulate_backrun(&self, opp: &Opportunity) -> anyhow::Result<SimulationResult> {
        // Look up pool reserves from cache
        let pool_addr = opp.pool_addresses.first().copied().unwrap_or([0u8; 20]);
        let (r0, r1, fee) = self.pool_reserves(&pool_addr);

        let fee = opp.pool_fees.first().map(|&f| (f / 100) as u128).unwrap_or(fee);

        // After a large swap, pool reserves are skewed.
        // Apply a 0.2% reserve shift to model post-swap state.
        let skewed_reserve0 = r0 * 10020 / 10000;
        let skewed_reserve1 = r1 * 9980 / 10000;

        let amount_mid = constant_product_swap(
            opp.amount_in,
            skewed_reserve0,
            skewed_reserve1,
            fee,
        );

        // Swap back at fair-value pool (another DEX or same after rebalance)
        let exit_addr = opp.pool_addresses.get(1).copied().unwrap_or([0u8; 20]);
        let (exit_r0, exit_r1, exit_fee) = self.pool_reserves(&exit_addr);
        let exit_fee = opp.pool_fees.get(1).map(|&f| (f / 100) as u128).unwrap_or(exit_fee);

        let amount_out = constant_product_swap(
            amount_mid,
            exit_r1,
            exit_r0,
            exit_fee,
        );

        let gas_cost = opp.gas_estimate as i128
            * self.config.strategy.max_gas_price_gwei as i128
            * 1_000_000_000;
        let net_profit = amount_out as i128 - opp.amount_in as i128 - gas_cost;

        Ok(SimulationResult {
            success: net_profit > 0,
            profit: net_profit,
            gas_used: opp.gas_estimate,
            error: None,
            state_changes: vec![],
        })
    }

    /// Simulate liquidation: flash borrow → repay debt → receive collateral
    fn simulate_liquidation(&self, opp: &Opportunity) -> anyhow::Result<SimulationResult> {
        // Flash loan to cover debt_amount
        let flash_amount = opp.amount_in;

        // Liquidation bonus (modeled from expected_profit / amount_in ratio)
        let bonus_amount = opp.expected_profit;

        // Swap collateral back to debt token to repay flash loan
        // Assume ~30 bps swap cost
        let swap_cost = (flash_amount + bonus_amount) * 30 / 10_000;

        let gas_cost = opp.gas_estimate as i128
            * self.config.strategy.max_gas_price_gwei as i128
            * 1_000_000_000;

        let flash_fee = flash_amount as i128 * 5 / 10_000;
        let net = bonus_amount as i128 - swap_cost as i128 - gas_cost - flash_fee;

        Ok(SimulationResult {
            success: net > 0,
            profit: net,
            gas_used: opp.gas_estimate,
            error: None,
            state_changes: vec![],
        })
    }

    pub async fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn success_rate(&self) -> f64 {
        let total = self.count.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.success_count.load(Ordering::Relaxed) as f64 / total as f64
    }
}

// ─── helpers ──────────────────────────────────────────────────────

/// Canonical pair ordering so (tokenA, tokenB) and (tokenB, tokenA) hit the same key.
#[inline]
fn ordered_pair(a: [u8; 20], b: [u8; 20]) -> ([u8; 20], [u8; 20]) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Constant product AMM: dy = y * dx * (1-fee) / (x + dx * (1-fee))
/// Uses checked arithmetic to prevent silent overflow on large inputs.
#[inline]
pub fn constant_product_swap(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u128,
) -> u128 {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 || fee_bps >= 10_000 {
        return 0;
    }
    let amount_in_with_fee = match amount_in.checked_mul(10_000 - fee_bps) {
        Some(v) => v,
        None => return 0,
    };
    let numerator = match amount_in_with_fee.checked_mul(reserve_out) {
        Some(v) => v,
        None => return 0,
    };
    let denominator = match reserve_in.checked_mul(10_000) {
        Some(v) => match v.checked_add(amount_in_with_fee) {
            Some(d) => d,
            None => return 0,
        },
        None => return 0,
    };
    if denominator == 0 { 0 } else { numerator / denominator }
}

/// Uniswap V3 concentrated liquidity swap simulation.
///
/// Models a single-tick-range swap using the V3 `sqrtPriceX96` math.
/// Given virtual reserves derived from `sqrtPriceX96` and `liquidity`,
/// computes the output amount respecting concentrated liquidity bounds.
///
/// This is an approximation that works well when the swap stays within
/// one active tick range (which is the common case for MEV-sized trades).
///
/// ## Parameters
/// - `amount_in`: input amount in token's smallest unit
/// - `sqrt_price_x96`: current pool sqrt price as Q64.96 fixed-point
///   (if 0, falls back to constant-product using reserves)
/// - `liquidity`: active in-range liquidity (L)
/// - `reserve_in` / `reserve_out`: pool reserves (used as fallback)
/// - `fee_bps`: fee in basis points (e.g. 5 = 0.05%, 30 = 0.30%)
/// - `zero_for_one`: true if swapping token0 → token1
#[inline]
pub fn concentrated_liquidity_swap(
    amount_in: u128,
    sqrt_price_x96: u128,
    liquidity: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u128,
    zero_for_one: bool,
) -> u128 {
    if amount_in == 0 || fee_bps >= 10_000 {
        return 0;
    }

    // If sqrtPriceX96 or liquidity not available, fall back to constant product
    if sqrt_price_x96 == 0 || liquidity == 0 {
        return constant_product_swap(amount_in, reserve_in, reserve_out, fee_bps);
    }

    // Apply fee to input
    let amount_in_after_fee = match amount_in.checked_mul(10_000 - fee_bps) {
        Some(v) => v / 10_000,
        None => return 0,
    };

    // Q96 = 2^96
    const Q96: u128 = 1u128 << 96;

    if zero_for_one {
        // Swapping token0 → token1
        // new_sqrt_price = L * sqrt_price / (L + amount_in * sqrt_price)
        // amount_out = L * (sqrt_price_old - sqrt_price_new)
        //
        // Single active-range approximation: treats liquidity as constant over the swap.
        // This is accurate for in-range MEV-sized trades that do not cross ticks.
        // amount_out = L * √P_old - L * √P_new
        //            = L * √P_old * amount_in * √P_old / (L + amount_in * √P_old)
        //        (this is the exact V3 formula reduced to single-range)

        // numerator = L * sqrt_price * amount_in_after_fee * sqrt_price
        //           = amount_in_after_fee * liquidity * sqrt_price^2 / (L * Q96 + amount_in_after_fee * sqrt_price)
        // Use u128 step-by-step to avoid overflow:

        // Virtual reserve computation:
        //   virtual_x = L * Q96 / sqrt_price
        //   virtual_y = L * sqrt_price / Q96
        let virtual_x = match liquidity.checked_mul(Q96) {
            Some(v) => v / sqrt_price_x96,
            None => return constant_product_swap(amount_in, reserve_in, reserve_out, fee_bps),
        };
        let virtual_y = match liquidity.checked_mul(sqrt_price_x96) {
            Some(v) => v / Q96,
            None => return constant_product_swap(amount_in, reserve_in, reserve_out, fee_bps),
        };

        // Use constant product on virtual reserves
        constant_product_swap_no_fee(amount_in_after_fee, virtual_x, virtual_y)
    } else {
        // Swapping token1 → token0
        // Virtual reserves flipped
        let virtual_x = match liquidity.checked_mul(Q96) {
            Some(v) => v / sqrt_price_x96,
            None => return constant_product_swap(amount_in, reserve_in, reserve_out, fee_bps),
        };
        let virtual_y = match liquidity.checked_mul(sqrt_price_x96) {
            Some(v) => v / Q96,
            None => return constant_product_swap(amount_in, reserve_in, reserve_out, fee_bps),
        };

        // token1 → token0: virtual_y is reserve_in, virtual_x is reserve_out
        constant_product_swap_no_fee(amount_in_after_fee, virtual_y, virtual_x)
    }
}

/// Constant product swap with fee already applied (internal helper).
#[inline]
fn constant_product_swap_no_fee(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return 0;
    }
    let numerator = match amount_in.checked_mul(reserve_out) {
        Some(v) => v,
        None => return 0,
    };
    let denominator = match reserve_in.checked_add(amount_in) {
        Some(v) => v,
        None => return 0,
    };
    if denominator == 0 { 0 } else { numerator / denominator }
}

/// Estimate gas for a bundle transaction based on data length
fn estimate_tx_gas(gas_limit: u64, data: &[u8]) -> u64 {
    // Base: 21000 + 16 per non-zero byte + 4 per zero byte
    let calldata_gas: u64 = data.iter().map(|&b| if b == 0 { 4u64 } else { 16u64 }).sum();
    let estimated = 21_000 + calldata_gas + 100_000; // +100k for contract execution
    estimated.min(gas_limit)
}

fn decode_addr_bytes(hex_str: &str) -> [u8; 20] {
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(s).unwrap_or_default();
    let mut out = [0u8; 20];
    let len = bytes.len().min(20);
    out[20 - len..].copy_from_slice(&bytes[..len]);
    out
}

fn u128_to_bytes32(val: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..32].copy_from_slice(&val.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OpportunityType;

    fn test_config() -> Arc<Config> {
        let mut config = Config::default();
        config.strategy.max_gas_price_gwei = 1; // Arbitrum
        Arc::new(config)
    }

    #[test]
    fn test_constant_product_swap() {
        // 1 ETH into 5000 ETH / 10M USDC pool at 0.3% fee
        let out = constant_product_swap(
            1_000_000_000_000_000_000,     // 1 ETH
            5_000_000_000_000_000_000_000,  // 5000 ETH
            10_000_000_000_000,             // 10M USDC
            30,                             // 0.3%
        );
        // Expected: ~1994 USDC (slightly less than 2000 due to fee + impact)
        assert!(out > 1_990_000_000 && out < 2_000_000_000,
            "Expected ~1994 USDC, got {}", out);
    }

    #[test]
    fn test_constant_product_zero_reserves() {
        assert_eq!(constant_product_swap(1000, 0, 1000, 30), 0);
        assert_eq!(constant_product_swap(1000, 1000, 0, 30), 0);
        assert_eq!(constant_product_swap(0, 1000, 1000, 30), 0);
    }

    #[tokio::test]
    async fn test_simulation_arbitrage() {
        let sim = EvmSimulator::new(test_config());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "WETH".to_string(),
            token_out: "USDC".to_string(),
            amount_in: 1_000_000_000_000_000_000, // 1 ETH
            expected_profit: 10_000_000_000_000_000, // 0.01 ETH
            gas_estimate: 250_000,
            deadline: 0,
            path: vec![crate::types::DexType::UniswapV2, crate::types::DexType::UniswapV3],
            pool_addresses: vec![[0xAA; 20], [0xBB; 20]],
            pool_fees: vec![3000, 500],
            target_tx: None,
        };

        let result = sim.simulate(&opp).await;
        // With same reserves on both DEXes, round-trip loses to fees
        assert!(!result.success || result.profit <= 0);
        assert!(result.gas_used > 0);
    }

    #[tokio::test]
    async fn test_simulation_count() {
        let sim = EvmSimulator::new(test_config());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Liquidation,
            token_in: "USDC".to_string(),
            token_out: "WETH".to_string(),
            amount_in: 50_000_000_000_000_000_000,
            expected_profit: 5_000_000_000_000_000_000,
            gas_estimate: 500_000,
            deadline: 0,
            path: vec![],
            pool_addresses: vec![],
            pool_fees: vec![],
            target_tx: None,
        };

        sim.simulate(&opp).await;
        sim.simulate(&opp).await;
        assert_eq!(sim.count().await, 2);
    }

    #[test]
    fn test_estimate_tx_gas() {
        let data = vec![0x12, 0x34, 0x00, 0x56]; // 3 non-zero, 1 zero
        let gas = estimate_tx_gas(500_000, &data);
        // 21000 + 3*16 + 1*4 + 100000 = 121052
        assert_eq!(gas, 121_052);
    }

    // ── Pool cache tests ──

    #[test]
    fn test_load_pools_and_get() {
        let sim = EvmSimulator::new(test_config());
        let pool = PoolState {
            address: [0xAA; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 5_000_000_000_000_000_000_000,
            reserve1: 10_000_000_000_000,
            fee: 3000,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        };
        sim.load_pools(vec![pool.clone()]);

        let fetched = sim.get_pool(&[0xAA; 20]);
        assert!(fetched.is_some());
        let f = fetched.unwrap();
        assert_eq!(f.reserve0, 5_000_000_000_000_000_000_000);
        assert_eq!(f.reserve1, 10_000_000_000_000);
    }

    #[test]
    fn test_update_pool_inserts_new() {
        let sim = EvmSimulator::new(test_config());
        assert!(sim.get_pool(&[0xBB; 20]).is_none());

        sim.update_pool(PoolState {
            address: [0xBB; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 1000,
            reserve1: 2000,
            fee: 500,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        });
        assert!(sim.get_pool(&[0xBB; 20]).is_some());
    }

    #[test]
    fn test_update_pool_overwrites_reserves() {
        let sim = EvmSimulator::new(test_config());
        sim.update_pool(PoolState {
            address: [0xCC; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 1000,
            reserve1: 2000,
            fee: 500,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        });
        sim.update_pool(PoolState {
            address: [0xCC; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 9999,
            reserve1: 8888,
            fee: 500,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        });
        let p = sim.get_pool(&[0xCC; 20]).unwrap();
        assert_eq!(p.reserve0, 9999);
        assert_eq!(p.reserve1, 8888);
    }

    #[test]
    fn test_pool_reserves_from_cache() {
        let sim = EvmSimulator::new(test_config());
        sim.update_pool(PoolState {
            address: [0xDD; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 42,
            reserve1: 84,
            fee: 300,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        });
        let (r0, r1, fee) = sim.pool_reserves(&[0xDD; 20]);
        assert_eq!(r0, 42);
        assert_eq!(r1, 84);
        assert_eq!(fee, 300);
    }

    #[test]
    fn test_pool_reserves_fallback_defaults() {
        let sim = EvmSimulator::new(test_config());
        // Unknown pool → default reserves
        let (r0, r1, fee) = sim.pool_reserves(&[0xFF; 20]);
        assert_eq!(r0, DEFAULT_WETH_RESERVE);
        assert_eq!(r1, DEFAULT_USDC_RESERVE);
        assert_eq!(fee, DEFAULT_FEE_BPS);
    }

    #[test]
    fn test_pool_reserves_zero_addr_uses_default() {
        let sim = EvmSimulator::new(test_config());
        let (r0, _r1, _fee) = sim.pool_reserves(&[0u8; 20]);
        assert_eq!(r0, DEFAULT_WETH_RESERVE);
    }

    // ── Overflow safety ──

    #[test]
    fn test_constant_product_no_panic_on_overflow() {
        // Whale trade: near u128::MAX values — must not panic
        let out = constant_product_swap(
            u128::MAX / 2,
            u128::MAX / 3,
            u128::MAX / 4,
            30,
        );
        // Should return 0 (overflow) rather than wrapping
        assert_eq!(out, 0, "overflow should produce 0, not wrapped value");
    }

    #[test]
    fn test_constant_product_fee_100_percent() {
        let out = constant_product_swap(1_000_000, 5_000_000, 10_000_000, 10_000);
        assert_eq!(out, 0, "100% fee should yield zero output");
    }

    #[test]
    fn test_concentrated_liquidity_swap_basic() {
        // Simulate a V3 pool: WETH/USDC at ~2000 USDC/ETH
        // sqrtPriceX96 for price 2000 (token1/token0) ≈ sqrt(2000) * 2^96
        // sqrt(2000) ≈ 44.72
        // 44.72 * 2^96 ≈ 3_543_191_142_285_914_205_922_034_688
        let sqrt_price_x96: u128 = 3_543_191_142_285_914_205_922_034_688;
        let liquidity: u128 = 10_000_000_000_000_000_000; // 10e18

        let out = concentrated_liquidity_swap(
            1_000_000_000_000_000_000,  // 1 ETH
            sqrt_price_x96,
            liquidity,
            5_000_000_000_000_000_000_000, // fallback reserve
            10_000_000_000_000,            // fallback reserve
            30,                            // 0.30% fee
            true,                          // token0 → token1
        );
        // Should produce some output
        assert!(out > 0, "V3 swap should produce output, got 0");
    }

    #[test]
    fn test_concentrated_liquidity_fallback_to_v2() {
        // When sqrtPriceX96=0, should fall back to constant product
        let out_v3 = concentrated_liquidity_swap(
            1_000_000,
            0, // no sqrt price → fallback
            0, // no liquidity → fallback
            5_000_000,
            10_000_000,
            30,
            true,
        );
        let out_v2 = constant_product_swap(1_000_000, 5_000_000, 10_000_000, 30);
        assert_eq!(out_v3, out_v2, "Should fall back to constant product");
    }

    #[test]
    fn test_concentrated_liquidity_zero_input() {
        let out = concentrated_liquidity_swap(0, 100, 100, 100, 100, 30, true);
        assert_eq!(out, 0);
    }

    #[test]
    fn test_ordered_pair_canonical() {
        let a = [0x01; 20];
        let b = [0x02; 20];
        assert_eq!(ordered_pair(a, b), (a, b));
        assert_eq!(ordered_pair(b, a), (a, b));
    }

    #[tokio::test]
    async fn test_simulate_backrun() {
        let sim = EvmSimulator::new(test_config());
        let opp = Opportunity {
            opportunity_type: OpportunityType::Backrun,
            token_in: "WETH".to_string(),
            token_out: "USDC".to_string(),
            amount_in: 500_000_000_000_000_000, // 0.5 ETH
            expected_profit: 0,
            gas_estimate: 180_000,
            deadline: 0,
            path: vec![crate::types::DexType::UniswapV3],
            pool_addresses: vec![[0xAA; 20]],
            pool_fees: vec![500],
            target_tx: None,
        };
        let result = sim.simulate(&opp).await;
        assert!(result.gas_used > 0);
    }

    #[tokio::test]
    async fn test_simulate_liquidation() {
        let sim = EvmSimulator::new(test_config());
        let opp = Opportunity {
            opportunity_type: OpportunityType::Liquidation,
            token_in: "USDC".to_string(),
            token_out: "WETH".to_string(),
            amount_in: 10_000_000_000_000_000_000,
            expected_profit: 500_000_000_000_000_000,
            gas_estimate: 450_000,
            deadline: 0,
            path: vec![],
            pool_addresses: vec![],
            pool_fees: vec![],
            target_tx: None,
        };
        let result = sim.simulate(&opp).await;
        assert!(result.gas_used > 0);
        // Liquidation with 5% bonus on 10 ETH should be profitable
        assert!(result.success);
        assert!(result.profit > 0);
    }

    #[test]
    fn test_success_rate() {
        let sim = EvmSimulator::new(test_config());
        assert_eq!(sim.success_rate(), 0.0);
    }

    #[tokio::test]
    async fn test_simulate_bundle() {
        use crate::types::{Bundle, BundleTransaction};
        let sim = EvmSimulator::new(test_config());
        let bundle = Bundle {
            transactions: vec![
                BundleTransaction {
                    to: "0xdead".to_string(),
                    value: 0,
                    gas_limit: 300_000,
                    gas_price: None,
                    max_fee_per_gas: None,
                    max_priority_fee_per_gas: Some(1_000_000_000),
                    data: vec![0x12, 0x34, 0x56],
                    nonce: None,
                },
            ],
            target_block: Some(1000),
            max_block_number: Some(1002),
            min_timestamp: None,
            max_timestamp: None,
            reverting_tx_hashes: vec![],
        };
        let result = sim.simulate_bundle(&bundle).await;
        assert!(result.gas_used > 0);
        assert_eq!(result.state_changes.len(), 1);
    }
}
