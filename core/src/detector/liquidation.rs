//! Liquidation detection — monitors under-collateralized lending positions
//!
//! Tracks positions across Aave V3, Compound V3, and Morpho via event
//! subscriptions, identifies positions whose health factor has dropped
//! below 1.0, and constructs profitable liquidation opportunities
//! using flash loans.

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, DexType};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::{debug, info};

/// Position data from lending protocol
#[derive(Debug, Clone)]
pub struct Position {
    pub user: [u8; 20],
    pub protocol: LendingProtocol,
    pub collateral_token: [u8; 20],
    pub collateral_amount: u128,
    pub debt_token: [u8; 20],
    pub debt_amount: u128,
    /// 18-decimal fixed point. < 1e18 means liquidatable
    pub health_factor: u128,
    pub last_updated: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LendingProtocol {
    AaveV3,
    CompoundV3,
    Morpho,
}

impl LendingProtocol {
    /// Liquidation bonus in basis points (protocol-specific)
    fn liquidation_bonus_bps(&self) -> u128 {
        match self {
            LendingProtocol::AaveV3 => 500,      // 5.0%
            LendingProtocol::CompoundV3 => 800,   // 8.0%
            LendingProtocol::Morpho => 500,        // 5.0%
        }
    }

    /// Maximum debt that can be liquidated in one call (fraction of total)
    fn close_factor(&self) -> u128 {
        match self {
            LendingProtocol::AaveV3 => 5000,     // 50%
            LendingProtocol::CompoundV3 => 5000,  // 50%
            LendingProtocol::Morpho => 10000,      // 100% (full liquidation)
        }
    }

    /// Flash loan fee in basis points
    fn flash_loan_fee_bps(&self) -> u128 {
        match self {
            LendingProtocol::AaveV3 => 5,     // 0.05% (Aave flash)
            LendingProtocol::CompoundV3 => 0,  // Balancer flash = 0
            LendingProtocol::Morpho => 0,       // Balancer flash = 0
        }
    }

    fn gas_estimate(&self) -> u64 {
        match self {
            LendingProtocol::AaveV3 => 450_000,
            LendingProtocol::CompoundV3 => 500_000,
            LendingProtocol::Morpho => 400_000,
        }
    }
}

/// Liquidation detector for lending protocols
pub struct LiquidationDetector {
    config: Arc<Config>,
    /// Active positions indexed by user address
    positions: Arc<RwLock<HashMap<[u8; 20], Vec<Position>>>>,
    /// Running count of liquidatable positions found
    found_count: std::sync::atomic::AtomicU64,
}

impl LiquidationDetector {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            positions: Arc::new(RwLock::new(HashMap::new())),
            found_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Bulk update tracked positions (e.g. from event indexer)
    pub fn update_positions(&self, new_positions: Vec<Position>) {
        let mut map = self.positions.write();
        for pos in new_positions {
            map.entry(pos.user).or_insert_with(Vec::new).push(pos);
        }
    }

    /// Remove stale positions older than max_age_seconds
    pub fn prune_stale(&self, now: u64, max_age_seconds: u64) {
        let mut map = self.positions.write();
        for positions in map.values_mut() {
            positions.retain(|p| now.saturating_sub(p.last_updated) < max_age_seconds);
        }
        map.retain(|_, v| !v.is_empty());
    }

    /// Scan all tracked positions and return profitable liquidation opportunities
    pub fn find_liquidatable(&self) -> Vec<Opportunity> {
        let map = self.positions.read();
        let mut opportunities = Vec::new();

        for positions in map.values() {
            for position in positions {
                // Health factor < 1e18 → liquidatable
                if position.health_factor < 1_000_000_000_000_000_000 {
                    if let Some(opp) = self.evaluate_liquidation(position) {
                        self.found_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        opportunities.push(opp);
                    }
                }
            }
        }

        // Sort by profit descending — execute most profitable first
        opportunities.sort_by(|a, b| b.expected_profit.cmp(&a.expected_profit));
        opportunities
    }

    /// Evaluate whether a single position liquidation is profitable
    fn evaluate_liquidation(&self, position: &Position) -> Option<Opportunity> {
        let protocol = &position.protocol;

        // Max repayable debt = debt_amount × close_factor
        let close_factor = protocol.close_factor();
        let max_repay = position.debt_amount * close_factor / 10_000;

        // Collateral received = repay_amount × (1 + bonus)
        let bonus_bps = protocol.liquidation_bonus_bps();
        let collateral_received = max_repay * (10_000 + bonus_bps) / 10_000;

        // Ensure we actually receive enough collateral
        if collateral_received > position.collateral_amount {
            // Position doesn't have enough collateral for full liquidation
            return None;
        }

        // Gross profit = bonus portion of collateral
        let gross_profit = max_repay * bonus_bps / 10_000;

        // Flash loan fee
        let flash_fee = max_repay * protocol.flash_loan_fee_bps() / 10_000;

        // Gas cost
        let gas = protocol.gas_estimate();
        let gas_price_wei = self.config.strategy.max_gas_price_gwei as u128 * 1_000_000_000;
        let gas_cost = gas as u128 * gas_price_wei;

        // DEX swap cost (collateral → debt token): ~30 bps
        let swap_cost = collateral_received * 30 / 10_000;

        // Net profit
        let net_profit = gross_profit
            .checked_sub(flash_fee)?
            .checked_sub(gas_cost)?
            .checked_sub(swap_cost)?;

        if net_profit < self.config.strategy.min_profit_wei {
            return None;
        }

        debug!(
            protocol = ?position.protocol,
            health = position.health_factor,
            debt = position.debt_amount,
            bonus_bps,
            net_profit,
            "Liquidation opportunity found"
        );

        Some(Opportunity {
            opportunity_type: OpportunityType::Liquidation,
            token_in: format!("0x{}", hex::encode(position.debt_token)),
            token_out: format!("0x{}", hex::encode(position.collateral_token)),
            amount_in: max_repay,
            expected_profit: net_profit,
            gas_estimate: gas,
            deadline: u64::MAX, // Liquidations are valid as long as health < 1
            path: vec![DexType::UniswapV3],
            target_tx: None,
        })
    }

    /// Statistics
    pub fn found_count(&self) -> u64 {
        self.found_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn tracked_positions(&self) -> usize {
        self.positions.read().values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Arc<Config> {
        let mut config = Config::default();
        config.strategy.min_profit_wei = 100_000_000_000_000; // 0.0001 ETH
        config.strategy.max_gas_price_gwei = 1;
        Arc::new(config)
    }

    #[test]
    fn test_liquidatable_position_detected() {
        let detector = LiquidationDetector::new(test_config());

        detector.update_positions(vec![Position {
            user: [1u8; 20],
            protocol: LendingProtocol::AaveV3,
            collateral_token: [0xAA; 20],
            collateral_amount: 100_000_000_000_000_000_000, // 100 ETH worth
            debt_token: [0xBB; 20],
            debt_amount: 80_000_000_000_000_000_000, // 80 ETH
            health_factor: 900_000_000_000_000_000,   // 0.9 — liquidatable
            last_updated: 0,
        }]);

        let opps = detector.find_liquidatable();
        assert_eq!(opps.len(), 1);
        assert!(opps[0].expected_profit > 0);
        assert!(matches!(opps[0].opportunity_type, OpportunityType::Liquidation));
    }

    #[test]
    fn test_healthy_position_skipped() {
        let detector = LiquidationDetector::new(test_config());

        detector.update_positions(vec![Position {
            user: [2u8; 20],
            protocol: LendingProtocol::CompoundV3,
            collateral_token: [0xCC; 20],
            collateral_amount: 200_000_000_000_000_000_000,
            debt_token: [0xDD; 20],
            debt_amount: 50_000_000_000_000_000_000,
            health_factor: 2_000_000_000_000_000_000, // 2.0 — healthy
            last_updated: 0,
        }]);

        let opps = detector.find_liquidatable();
        assert!(opps.is_empty());
    }

    #[test]
    fn test_close_factor_limits_repay() {
        // Aave close factor = 50%
        let protocol = LendingProtocol::AaveV3;
        assert_eq!(protocol.close_factor(), 5000);

        let debt = 100_000_000_000_000_000_000u128; // 100 ETH
        let max_repay = debt * protocol.close_factor() / 10_000;
        assert_eq!(max_repay, 50_000_000_000_000_000_000); // 50 ETH
    }

    #[test]
    fn test_prune_stale() {
        let detector = LiquidationDetector::new(test_config());

        detector.update_positions(vec![
            Position {
                user: [1u8; 20],
                protocol: LendingProtocol::AaveV3,
                collateral_token: [0; 20],
                collateral_amount: 0,
                debt_token: [0; 20],
                debt_amount: 0,
                health_factor: 0,
                last_updated: 100,
            },
        ]);

        assert_eq!(detector.tracked_positions(), 1);
        detector.prune_stale(200, 50); // max_age=50, now=200, pos=100 → stale
        assert_eq!(detector.tracked_positions(), 0);
    }
}
