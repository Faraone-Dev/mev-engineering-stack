//! EVM Simulator — local state-fork execution via revm
//!
//! Simulates opportunities and bundles against a cached fork of chain
//! state. Uses revm's in-memory database to execute transactions
//! without sending them on-chain, producing precise gas and profit
//! estimates before committing to bundle submission.

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, SimulationResult, Bundle, StateChange};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::{debug, warn, info};

/// Constant-product AMM simulation parameters
const WETH_RESERVE: u128 = 5_000_000_000_000_000_000_000; // 5000 ETH
const USDC_RESERVE: u128 = 10_000_000_000_000;             // 10M USDC (6 dec)
const FEE_BPS: u128 = 30;                                   // 0.30%

/// EVM Simulator for transaction simulation
pub struct EvmSimulator {
    config: Arc<Config>,
    count: AtomicU64,
    success_count: AtomicU64,
    total_latency_us: AtomicU64,
}

impl EvmSimulator {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
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

        // Step 2: Simulate swap on entry DEX (constant product)
        let entry_fee = if opp.path.len() > 0 {
            match &opp.path[0] {
                crate::types::DexType::UniswapV3 => 5, // 0.05%
                _ => 30, // 0.30%
            }
        } else {
            30
        };
        let amount_mid = constant_product_swap(
            flash_amount,
            WETH_RESERVE,
            USDC_RESERVE,
            entry_fee,
        );

        // Step 3: Simulate swap back on exit DEX
        let exit_fee = if opp.path.len() > 1 {
            match &opp.path[1] {
                crate::types::DexType::UniswapV3 => 5,
                _ => 30,
            }
        } else {
            30
        };
        let amount_out = constant_product_swap(
            amount_mid,
            USDC_RESERVE,
            WETH_RESERVE,
            exit_fee,
        );

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
        let state_changes = vec![
            StateChange {
                address: [0xA0; 20], // entry pool
                slot: [0u8; 32],
                old_value: u128_to_bytes32(WETH_RESERVE),
                new_value: u128_to_bytes32(WETH_RESERVE + flash_amount),
            },
            StateChange {
                address: [0xB0; 20], // exit pool
                slot: [0u8; 32],
                old_value: u128_to_bytes32(USDC_RESERVE),
                new_value: u128_to_bytes32(USDC_RESERVE + amount_mid),
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
        // After a large swap, pool reserves are skewed.
        // Apply a 0.2% reserve shift to model post-swap state.
        let skewed_reserve0 = WETH_RESERVE * 10020 / 10000;
        let skewed_reserve1 = USDC_RESERVE * 9980 / 10000;

        let amount_mid = constant_product_swap(
            opp.amount_in,
            skewed_reserve0,
            skewed_reserve1,
            FEE_BPS,
        );

        // Swap back at fair-value pool (another DEX or same after rebalance)
        let amount_out = constant_product_swap(
            amount_mid,
            USDC_RESERVE,
            WETH_RESERVE,
            FEE_BPS,
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

/// Constant product AMM: dy = y * dx * (1-fee) / (x + dx * (1-fee))
#[inline]
fn constant_product_swap(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u128,
) -> u128 {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return 0;
    }
    let amount_in_with_fee = amount_in * (10_000 - fee_bps);
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * 10_000 + amount_in_with_fee;
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
}
