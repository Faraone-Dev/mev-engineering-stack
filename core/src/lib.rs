//! MEV Core Engine
//! 
//! High-performance MEV extraction engine with sub-millisecond latency.
//! 
//! ## Architecture
//! - FFI: C hot path bindings for SIMD/lock-free operations
//! - Mempool: Ultra-low latency WebSocket monitoring
//! - Detector: Multi-threaded work-stealing arbitrage detection
//! - Simulator: Two-stage pipeline — AMM math filter + REVM fork execution
//! - Builder: Bundle construction and submission

#![allow(unused_imports)]

pub mod config;
pub mod detector;
pub mod simulator;
pub mod builder;
pub mod ffi;
pub mod types;
pub mod mempool;
pub mod bench;
pub mod grpc;

// Arbitrum-specific modules
pub mod arbitrum;

pub use config::Config;
pub use detector::OpportunityDetector;
pub use simulator::EvmSimulator;
pub use simulator::evm::{EvmForkSimulator, ForkDB, BlockContext};
pub use builder::BundleBuilder;
pub use mempool::{MempoolMonitor, MempoolConfig, MempoolTx};
pub use bench::run_all_benchmarks;

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, error};

/// MEV Engine orchestrates all components
#[derive(Clone)]
pub struct MevEngine {
    #[allow(dead_code)]
    config: Arc<Config>,
    detector: Arc<OpportunityDetector>,
    simulator: Arc<EvmSimulator>,
    builder: Arc<BundleBuilder>,
    running: Arc<RwLock<bool>>,
    start_time: Arc<RwLock<Option<Instant>>>,
}

impl MevEngine {
    /// Create new MEV Engine instance
    pub fn new(config: Config) -> Self {
        let config = Arc::new(config);
        
        Self {
            config: config.clone(),
            detector: Arc::new(OpportunityDetector::new(config.clone())),
            simulator: Arc::new(EvmSimulator::new(config.clone())),
            builder: Arc::new(BundleBuilder::new(config.clone())),
            running: Arc::new(RwLock::new(false)),
            start_time: Arc::new(RwLock::new(None)),
        }
    }

    /// Start the MEV engine
    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Starting MEV Engine...");
        
        *self.running.write().await = true;
        *self.start_time.write().await = Some(Instant::now());

        // Start components
        self.detector.start().await?;
        self.simulator.start().await?;
        self.builder.start().await?;

        info!("MEV Engine started successfully");
        Ok(())
    }

    /// Stop the MEV engine
    pub async fn stop(&self) -> anyhow::Result<()> {
        info!("Stopping MEV Engine...");
        
        *self.running.write().await = false;

        self.detector.stop().await?;
        self.simulator.stop().await?;
        self.builder.stop().await?;

        info!("MEV Engine stopped");
        Ok(())
    }

    /// Check if engine is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// Get engine statistics
    pub async fn stats(&self) -> EngineStats {
        let uptime = self.start_time.read().await
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        EngineStats {
            opportunities_detected: self.detector.count().await,
            simulations_run: self.simulator.count().await,
            bundles_submitted: self.builder.count().await,
            uptime_seconds: uptime,
        }
    }
}

/// Engine statistics
#[derive(Debug, Clone)]
pub struct EngineStats {
    pub opportunities_detected: u64,
    pub simulations_run: u64,
    pub bundles_submitted: u64,
    pub uptime_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_engine_creation() {
        let config = Config::default();
        let engine = MevEngine::new(config);
        assert!(!engine.is_running().await);
    }
}
