// Arbitrum-specific MEV module

pub mod pools;
pub mod detector;
pub mod executor;

use ethers::types::{Address, U256};
use std::str::FromStr;

/// Arbitrum Chain Configuration
pub struct ArbitrumConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    pub ws_url: String,
    pub balancer_vault: Address,
    pub weth: Address,
    pub usdc: Address,
    pub usdt: Address,
    pub arb: Address,
}

impl Default for ArbitrumConfig {
    fn default() -> Self {
        Self {
            chain_id: 42161,
            rpc_url: String::new(),
            ws_url: String::new(),
            balancer_vault: Address::from_str("0xBA12222222228d8Ba445958a75a0704d566BF2C8").expect("invalid Balancer vault address"),
            weth: Address::from_str("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1").expect("invalid WETH address"),
            usdc: Address::from_str("0xaf88d065e77c8cC2239327C5EDb3A432268e5831").expect("invalid USDC address"),
            usdt: Address::from_str("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9").expect("invalid USDT address"),
            arb: Address::from_str("0x912CE59144191C1204E64559FE8253a0e49E6548").expect("invalid ARB address"),
        }
    }
}

/// DEX addresses on Arbitrum
pub struct ArbitrumDexes {
    // Uniswap V3
    pub uniswap_v3_factory: Address,
    pub uniswap_v3_router: Address,
    pub uniswap_v3_quoter: Address,
    
    // SushiSwap
    pub sushi_factory: Address,
    pub sushi_router: Address,
    
    // Camelot
    pub camelot_factory: Address,
    pub camelot_router: Address,
    
    // Balancer
    pub balancer_vault: Address,
}

impl Default for ArbitrumDexes {
    fn default() -> Self {
        Self {
            // Uniswap V3
            uniswap_v3_factory: Address::from_str("0x1F98431c8aD98523631AE4a59f267346ea31F984").unwrap(),
            uniswap_v3_router: Address::from_str("0xE592427A0AEce92De3Edee1F18E0157C05861564").unwrap(),
            uniswap_v3_quoter: Address::from_str("0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6").unwrap(),
            
            // SushiSwap V2
            sushi_factory: Address::from_str("0xc35DADB65012eC5796536bD9864eD8773aBc74C4").unwrap(),
            sushi_router: Address::from_str("0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506").unwrap(),
            
            // Camelot
            camelot_factory: Address::from_str("0x6EcCab422D763aC031210895C81787E87B43A652").unwrap(),
            camelot_router: Address::from_str("0xc873fEcbd354f5A56E00E710B90EF4201db2448d").unwrap(),
            
            // Balancer
            balancer_vault: Address::from_str("0xBA12222222228d8Ba445958a75a0704d566BF2C8").unwrap(),
        }
    }
}

/// Fee tiers for Uniswap V3
#[derive(Clone, Copy, Debug)]
pub enum UniV3FeeTier {
    Lowest = 100,   // 0.01%
    Low = 500,      // 0.05%
    Medium = 3000,  // 0.30%
    High = 10000,   // 1.00%
}

impl UniV3FeeTier {
    pub fn all() -> Vec<Self> {
        vec![Self::Lowest, Self::Low, Self::Medium, Self::High]
    }
    
    pub fn as_u32(&self) -> u32 {
        *self as u32
    }
}
