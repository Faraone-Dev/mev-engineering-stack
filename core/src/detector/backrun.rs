//! Backrun detection — captures price recovery after large swaps
//!
//! Detects large pending swaps that will push a pool's reserves out of
//! equilibrium, then calculates the profit from a backrun trade that
//! rides the price recovery toward the true market price.

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, PendingTx, DexType, PoolState};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::debug;

/// Known V2/V3 swap selectors that indicate a large trade
const SWAP_SELECTORS: [[u8; 4]; 8] = [
    [0x38, 0xed, 0x17, 0x39], // swapExactTokensForTokens
    [0x88, 0x03, 0xdb, 0xee], // swapTokensForExactTokens
    [0x7f, 0xf3, 0x6a, 0xb5], // swapExactETHForTokens
    [0x18, 0xcb, 0xaf, 0xe5], // swapExactTokensForETH
    [0x41, 0x4b, 0xf3, 0x89], // exactInputSingle
    [0xc0, 0x4b, 0x8d, 0x59], // exactInput
    [0xdb, 0x3e, 0x21, 0x98], // exactOutputSingle
    [0xfb, 0x3b, 0xdb, 0x41], // swapETHForExactTokens
];

/// Backrun detector for large swaps
pub struct BackrunDetector {
    config: Arc<Config>,
    /// Minimum swap value (wei) to consider for backrunning
    min_swap_size_wei: u128,
    /// Cached pool liquidity for impact estimation
    pool_liquidity: Arc<RwLock<HashMap<[u8; 20], u128>>>,
}

impl BackrunDetector {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            min_swap_size_wei: 10_000_000_000_000_000_000, // 10 ETH
            pool_liquidity: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update cached pool liquidity from pool refresh
    pub fn update_liquidity(&self, pool_addr: [u8; 20], total_liquidity_wei: u128) {
        self.pool_liquidity.write().insert(pool_addr, total_liquidity_wei);
    }

    /// Detect backrun opportunity
    pub async fn detect(&self, tx: &PendingTx) -> Option<Opportunity> {
        // Must be a swap
        if !self.is_swap(tx) {
            return None;
        }

        // Estimate effective swap value
        let swap_size = self.estimate_swap_size(tx)?;
        if swap_size < self.min_swap_size_wei {
            return None;
        }

        // Estimate pool liquidity and price impact
        let pool_addr = self.extract_router_target(tx)?;
        let pool_liquidity = self.get_pool_liquidity(&pool_addr);
        let price_impact_bps = self.calculate_price_impact(swap_size, pool_liquidity)?;

        // Skip tiny impacts — not worth the gas
        if price_impact_bps < 5 {
            return None;
        }

        let profit = self.calculate_backrun_profit(swap_size, price_impact_bps)?;
        if profit < self.config.strategy.min_profit_wei {
            return None;
        }

        // Optimal backrun size: sqrt(swap_size * liquidity) (simplified)
        let backrun_amount = (swap_size as f64 * 0.1).min(pool_liquidity as f64 * 0.01) as u128;
        let backrun_amount = backrun_amount.max(1);

        debug!(
            swap_size,
            price_impact_bps,
            profit,
            backrun_amount,
            "Backrun opportunity detected"
        );

        Some(Opportunity {
            opportunity_type: OpportunityType::Backrun,
            token_in: format!("0x{}", hex::encode(&pool_addr)),
            token_out: "TARGET".to_string(),
            amount_in: backrun_amount,
            expected_profit: profit,
            gas_estimate: 180_000,
            deadline: tx.timestamp + 12,
            path: vec![DexType::UniswapV3],
            target_tx: Some(tx.hash),
        })
    }

    /// Check if tx selector matches a known swap function
    fn is_swap(&self, tx: &PendingTx) -> bool {
        if tx.input.len() < 4 {
            return false;
        }
        let sel: [u8; 4] = tx.input[0..4].try_into().unwrap_or([0; 4]);
        SWAP_SELECTORS.contains(&sel)
    }

    /// Estimate swap value from calldata or tx value
    fn estimate_swap_size(&self, tx: &PendingTx) -> Option<u128> {
        // Native ETH swaps: value field
        if tx.value > 0 {
            return Some(tx.value);
        }

        // Token swaps: amountIn is first param after selector (4..36)
        if tx.input.len() >= 36 {
            // Read u128 from right-aligned 32-byte ABI word
            let amount = u128::from_be_bytes(
                tx.input[20..36].try_into().ok()?
            );
            if amount > 0 {
                return Some(amount);
            }
        }

        None
    }

    /// Extract the pool/router target address from the tx
    fn extract_router_target(&self, tx: &PendingTx) -> Option<[u8; 20]> {
        tx.to
    }

    /// Get cached or estimated pool liquidity
    fn get_pool_liquidity(&self, pool_addr: &[u8; 20]) -> u128 {
        self.pool_liquidity
            .read()
            .get(pool_addr)
            .copied()
            // Default: assume ~$5M equivalent liquidity
            .unwrap_or(2_500_000_000_000_000_000_000) // ~2500 ETH
    }

    /// Calculate price impact using constant-product invariant
    /// impact_bps = amount_in / (reserve + amount_in) * 10000
    fn calculate_price_impact(&self, swap_size: u128, pool_liquidity: u128) -> Option<u128> {
        if pool_liquidity == 0 {
            return None;
        }

        // For constant product: impact ≈ swap_size / (2 * reserve) * 10000
        // reserve ≈ pool_liquidity / 2 (one side of the pair)
        let reserve = pool_liquidity / 2;
        if reserve == 0 {
            return None;
        }

        let impact_bps = swap_size * 10_000 / (reserve + swap_size);
        Some(impact_bps.min(1000)) // Cap at 10%
    }

    /// Calculate net backrun profit
    /// Profit = swap_value × impact × recovery_rate − gas − slippage
    fn calculate_backrun_profit(&self, swap_size: u128, price_impact_bps: u128) -> Option<u128> {
        // Price recovery: we capture ~40-60% of the impact as it mean-reverts
        let recovery_rate_pct = 45u128;

        let gross = swap_size * price_impact_bps * recovery_rate_pct / (10_000 * 100);

        // Gas cost: 180k gas × configured max gas price
        let gas_price_wei = self.config.strategy.max_gas_price_gwei as u128 * 1_000_000_000;
        let gas_cost = 180_000u128 * gas_price_wei;

        // Slippage: configured tolerance
        let slippage_cost = gross * self.config.strategy.slippage_tolerance_bps as u128 / 10_000;

        gross.checked_sub(gas_cost)?.checked_sub(slippage_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Arc<Config> {
        let mut config = Config::default();
        config.strategy.min_profit_wei = 1_000_000_000_000_000; // 0.001 ETH
        config.strategy.max_gas_price_gwei = 1; // Arbitrum-level gas
        config.strategy.slippage_tolerance_bps = 50;
        Arc::new(config)
    }

    #[test]
    fn test_price_impact_calculation() {
        let detector = BackrunDetector::new(test_config());
        // 10 ETH swap into 2500 ETH pool → ~0.4% impact
        let impact = detector.calculate_price_impact(
            10_000_000_000_000_000_000,    // 10 ETH
            5_000_000_000_000_000_000_000,  // 5000 ETH total
        ).unwrap();
        assert!(impact >= 30 && impact <= 50, "Expected ~40 bps, got {}", impact);
    }

    #[test]
    fn test_small_swap_filtered() {
        let detector = BackrunDetector::new(test_config());
        // 0.5 ETH swap — below 10 ETH minimum
        let size = detector.estimate_swap_size(&PendingTx {
            hash: [0; 32],
            from: [0; 20],
            to: Some([0; 20]),
            value: 500_000_000_000_000_000, // 0.5 ETH
            gas_price: 0,
            gas_limit: 0,
            input: vec![0x38, 0xed, 0x17, 0x39],
            nonce: 0,
            timestamp: 0,
        });
        assert!(size.unwrap() < detector.min_swap_size_wei);
    }

    #[test]
    fn test_is_swap_selector() {
        let detector = BackrunDetector::new(test_config());
        let mut tx = PendingTx {
            hash: [0; 32], from: [0; 20], to: Some([0; 20]),
            value: 0, gas_price: 0, gas_limit: 0,
            input: vec![0x38, 0xed, 0x17, 0x39, 0x00], // V2 swap
            nonce: 0, timestamp: 0,
        };
        assert!(detector.is_swap(&tx));

        tx.input = vec![0x09, 0x5e, 0xa7, 0xb3, 0x00]; // approve — not a swap
        assert!(!detector.is_swap(&tx));
    }
}
