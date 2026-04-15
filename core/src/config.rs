//! Configuration module

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Main configuration struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Chain configurations
    pub chains: HashMap<u64, ChainConfig>,
    
    /// RPC endpoints
    pub rpc: RpcConfig,
    
    /// Strategy settings
    pub strategy: StrategyConfig,
    
    /// Performance settings
    pub performance: PerformanceConfig,
    
    /// Logging settings
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    pub name: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub ws_url: String,
    pub flashbots_relay: Option<String>,
    pub balancer_vault: String,
    pub contract_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub primary: Vec<String>,
    pub fallback: Vec<String>,
    pub max_connections: usize,
    pub request_timeout_ms: u64,
    pub retry_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub min_profit_wei: u128,
    pub min_profit_bps: u16,
    pub max_gas_price_gwei: u64,
    pub slippage_tolerance_bps: u16,
    pub enabled_strategies: Vec<String>,
    /// Minimum swap size (wei) to consider for backrunning
    #[serde(default = "default_min_backrun_swap_wei")]
    pub min_backrun_swap_wei: u128,
}

fn default_min_backrun_swap_wei() -> u128 {
    10_000_000_000_000_000_000 // 10 ETH
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    pub detector_threads: usize,
    pub simulator_threads: usize,
    pub max_pending_opportunities: usize,
    pub simulation_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub json_output: bool,
    pub file_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut chains = HashMap::new();
        
        chains.insert(1, ChainConfig {
            name: "Ethereum".to_string(),
            chain_id: 1,
            rpc_url: "https://eth.llamarpc.com".to_string(),
            ws_url: "wss://eth.llamarpc.com".to_string(),
            flashbots_relay: Some("https://relay.flashbots.net".to_string()),
            balancer_vault: "0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string(),
            contract_address: None,
        });

        chains.insert(42161, ChainConfig {
            name: "Arbitrum".to_string(),
            chain_id: 42161,
            rpc_url: "https://arb1.arbitrum.io/rpc".to_string(),
            ws_url: "wss://arb1.arbitrum.io/ws".to_string(),
            flashbots_relay: None,
            balancer_vault: "0xBA12222222228d8Ba445958a75a0704d566BF2C8".to_string(),
            contract_address: None,
        });

        Self {
            chains,
            rpc: RpcConfig {
                primary: vec![],
                fallback: vec![],
                max_connections: 10,
                request_timeout_ms: 5000,
                retry_count: 3,
            },
            strategy: StrategyConfig {
                min_profit_wei: 1_000_000_000_000_000, // 0.001 ETH
                min_profit_bps: 10,
                max_gas_price_gwei: 100,
                slippage_tolerance_bps: 50,
                enabled_strategies: vec![
                    "arbitrage".to_string(),
                    "backrun".to_string(),
                ],
                min_backrun_swap_wei: default_min_backrun_swap_wei(),
            },
            performance: PerformanceConfig {
                detector_threads: num_cpus::get(),
                simulator_threads: num_cpus::get() / 2,
                max_pending_opportunities: 1000,
                simulation_timeout_ms: 100,
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                json_output: true,
                file_path: Some("logs/mev-engine.log".to_string()),
            },
        }
    }
}

impl Config {
    /// Load config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();

        // Try to load from file first
        let config_path = std::env::var("MEV_CONFIG")
            .unwrap_or_else(|_| "config/config.json".to_string());

        let mut config = if std::path::Path::new(&config_path).exists() {
            let content = std::fs::read_to_string(&config_path)?;
            serde_json::from_str(&content)?
        } else {
            Config::default()
        };

        if let Some(arbitrum) = config.chains.get_mut(&42161) {
            if let Ok(rpc_url) = std::env::var("ARBITRUM_RPC_URL") {
                if !rpc_url.trim().is_empty() {
                    arbitrum.rpc_url = rpc_url;
                }
            }

            if let Ok(ws_url) = std::env::var("ARBITRUM_WS_URL") {
                if !ws_url.trim().is_empty() {
                    arbitrum.ws_url = ws_url;
                }
            }

            if let Ok(contract_address) = std::env::var("CONTRACT_ADDRESS") {
                if !contract_address.trim().is_empty() {
                    arbitrum.contract_address = Some(contract_address);
                }
            }

            if let Ok(relay_url) = std::env::var("FLASHBOTS_RELAY_URL") {
                if !relay_url.trim().is_empty() {
                    arbitrum.flashbots_relay = Some(relay_url);
                }
            }
        }

        Ok(config)
    }

    /// Save config to file
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_has_ethereum() {
        let cfg = Config::default();
        let eth = cfg.chains.get(&1).expect("chain 1 must exist");
        assert_eq!(eth.name, "Ethereum");
        assert_eq!(eth.chain_id, 1);
        assert!(eth.flashbots_relay.is_some());
    }

    #[test]
    fn test_default_config_has_arbitrum() {
        let cfg = Config::default();
        let arb = cfg.chains.get(&42161).expect("chain 42161 must exist");
        assert_eq!(arb.name, "Arbitrum");
        assert!(arb.flashbots_relay.is_none());
    }

    #[test]
    fn test_serde_roundtrip() {
        let original = Config::default();
        let json = serde_json::to_string(&original).expect("serialize");
        let deserialized: Config = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.chains.len(), original.chains.len());
        assert_eq!(deserialized.rpc.max_connections, original.rpc.max_connections);
        assert_eq!(deserialized.strategy.min_profit_wei, original.strategy.min_profit_wei);
        assert_eq!(deserialized.strategy.min_profit_bps, original.strategy.min_profit_bps);
        assert_eq!(deserialized.performance.max_pending_opportunities, 1000);
        assert_eq!(deserialized.logging.level, "info");
        assert_eq!(deserialized.logging.json_output, true);
    }

    #[test]
    fn test_serde_chain_config_roundtrip() {
        let chain = ChainConfig {
            name: "TestChain".to_string(),
            chain_id: 999,
            rpc_url: "http://localhost:8545".to_string(),
            ws_url: "ws://localhost:8546".to_string(),
            flashbots_relay: None,
            balancer_vault: "0x0000000000000000000000000000000000000000".to_string(),
            contract_address: Some("0xdead".to_string()),
        };
        let json = serde_json::to_string(&chain).expect("serialize chain");
        let back: ChainConfig = serde_json::from_str(&json).expect("deserialize chain");
        assert_eq!(back.chain_id, 999);
        assert_eq!(back.contract_address, Some("0xdead".to_string()));
    }

    #[test]
    fn test_from_env_fallback_to_default() {
        // No MEV_CONFIG env var, no file → should fall back to default
        std::env::remove_var("MEV_CONFIG");
        let cfg = Config::from_env().expect("from_env should succeed with defaults");
        assert!(cfg.chains.contains_key(&1));
        assert!(cfg.chains.contains_key(&42161));
    }

    #[test]
    fn test_save_and_reload() {
        let dir = std::env::temp_dir().join("mev_config_test");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("test_config.json");
        let path_str = path.to_str().unwrap();

        let original = Config::default();
        original.save(path_str).expect("save should succeed");

        let content = std::fs::read_to_string(path_str).expect("read file");
        let loaded: Config = serde_json::from_str(&content).expect("parse saved config");
        assert_eq!(loaded.strategy.min_profit_wei, original.strategy.min_profit_wei);
        assert_eq!(loaded.rpc.retry_count, 3);

        // Clean up
        std::fs::remove_file(path_str).ok();
    }

    #[test]
    fn test_default_strategy_has_arbitrage_and_backrun() {
        let cfg = Config::default();
        assert!(cfg.strategy.enabled_strategies.contains(&"arbitrage".to_string()));
        assert!(cfg.strategy.enabled_strategies.contains(&"backrun".to_string()));
    }

    #[test]
    fn test_performance_defaults_nonzero() {
        let cfg = Config::default();
        assert!(cfg.performance.detector_threads > 0);
        assert!(cfg.performance.simulation_timeout_ms > 0);
        assert_eq!(cfg.performance.max_pending_opportunities, 1000);
    }
}
