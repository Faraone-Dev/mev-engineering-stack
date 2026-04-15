//! Opportunity detection module

mod arbitrage;
mod backrun;
mod liquidation;
pub mod multi_threaded;
pub mod sandwich;

pub use arbitrage::ArbitrageDetector;
pub use backrun::BackrunDetector;
pub use liquidation::LiquidationDetector;
pub use multi_threaded::{
    MultiThreadedDetector,
    DetectorConfig,
    DetectorStats,
    Opportunity as MTOpportunity,
    PathStep,
    DexType,
    PoolState,
};

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, PendingTx};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, debug, warn};

/// Main opportunity detector
pub struct OpportunityDetector {
    config: Arc<Config>,
    arbitrage: ArbitrageDetector,
    backrun: BackrunDetector,
    liquidation: LiquidationDetector,
    count: AtomicU64,
}

impl OpportunityDetector {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config: config.clone(),
            arbitrage: ArbitrageDetector::new(config.clone()),
            backrun: BackrunDetector::new(config.clone()),
            liquidation: LiquidationDetector::new(config.clone()),
            count: AtomicU64::new(0),
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Starting opportunity detector");
        Ok(())
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        info!("Stopping opportunity detector");
        Ok(())
    }

    /// Process a pending transaction
    pub async fn process_tx(&self, tx: PendingTx) -> Vec<Opportunity> {
        let mut opportunities = Vec::new();
        let enabled = &self.config.strategy.enabled_strategies;

        // Check for arbitrage
        if enabled.iter().any(|s| s == "arbitrage") {
            if let Some(opp) = self.arbitrage.detect(&tx).await {
                debug!("Arbitrage opportunity found: {:?}", opp);
                opportunities.push(opp);
            }
        }

        // Check for backrun
        if enabled.iter().any(|s| s == "backrun") {
            if let Some(opp) = self.backrun.detect(&tx).await {
                debug!("Backrun opportunity found: {:?}", opp);
                opportunities.push(opp);
            }
        }

        // Check for liquidation opportunities
        if enabled.iter().any(|s| s == "liquidation") {
            let liquidations = self.liquidation.find_liquidatable();
            for opp in liquidations {
                debug!("Liquidation opportunity found: {:?}", opp);
                opportunities.push(opp);
            }
        }

        // Update count
        self.count.fetch_add(opportunities.len() as u64, Ordering::Relaxed);

        opportunities
    }

    /// Get total opportunities detected
    pub async fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}
