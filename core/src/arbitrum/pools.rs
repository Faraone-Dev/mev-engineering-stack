// Pool Discovery for Arbitrum
// Finds all trading pools across Uniswap V3, SushiSwap, Camelot

use ethers::{
    prelude::*,
    types::{Address, H256, U256},
    providers::{Provider, Http},
    contract::abigen,
};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::warn;

// Generate contract bindings
abigen!(
    UniswapV3Factory,
    r#"[
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool)
        event PoolCreated(address indexed token0, address indexed token1, uint24 indexed fee, int24 tickSpacing, address pool)
    ]"#
);

abigen!(
    UniswapV3Pool,
    r#"[
        function token0() external view returns (address)
        function token1() external view returns (address)
        function fee() external view returns (uint24)
        function liquidity() external view returns (uint128)
        function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked)
    ]"#
);

abigen!(
    UniswapV2Factory,
    r#"[
        function getPair(address tokenA, address tokenB) external view returns (address pair)
        function allPairs(uint256) external view returns (address pair)
        function allPairsLength() external view returns (uint256)
    ]"#
);

abigen!(
    UniswapV2Pair,
    r#"[
        function token0() external view returns (address)
        function token1() external view returns (address)
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
    ]"#
);

/// Pool types
#[derive(Clone, Debug)]
pub enum PoolType {
    UniswapV3 { fee: u32 },
    SushiSwap,
    Camelot,
}

/// Unified pool representation
#[derive(Clone, Debug)]
pub struct Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub pool_type: PoolType,
    pub reserve0: U256,
    pub reserve1: U256,
    pub liquidity: U256,
    pub fee_bps: u32, // in basis points (30 = 0.30%)
}

impl Pool {
    /// Calculate output amount for V2-style pools
    pub fn get_amount_out(&self, amount_in: U256, token_in: Address) -> U256 {
        let (reserve_in, reserve_out) = if token_in == self.token0 {
            (self.reserve0, self.reserve1)
        } else {
            (self.reserve1, self.reserve0)
        };
        
        if reserve_in.is_zero() || reserve_out.is_zero() {
            return U256::zero();
        }
        
        // AMM formula: amount_out = (amount_in * fee_factor * reserve_out) / (reserve_in + amount_in * fee_factor)
        let fee_factor = 10000 - self.fee_bps;
        let amount_in_with_fee = amount_in * fee_factor;
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * 10000 + amount_in_with_fee;
        
        numerator / denominator
    }
    
    /// Get price of token0 in terms of token1
    pub fn get_price(&self) -> f64 {
        if self.reserve0.is_zero() {
            return 0.0;
        }
        
        let r0 = self.reserve0.as_u128() as f64;
        let r1 = self.reserve1.as_u128() as f64;
        r1 / r0
    }
}

/// Pool discovery and management
pub struct PoolManager {
    provider: Arc<Provider<Http>>,
    pools: RwLock<HashMap<Address, Pool>>,
    
    // Factory addresses
    uniswap_v3_factory: Address,
    sushi_factory: Address,
    camelot_factory: Address,
}

impl PoolManager {
    pub fn new(provider: Arc<Provider<Http>>) -> Self {
        use std::str::FromStr;
        
        Self {
            provider,
            pools: RwLock::new(HashMap::new()),
            uniswap_v3_factory: Address::from_str("0x1F98431c8aD98523631AE4a59f267346ea31F984").expect("invalid Uniswap V3 factory"),
            sushi_factory: Address::from_str("0xc35DADB65012eC5796536bD9864eD8773aBc74C4").expect("invalid SushiSwap factory"),
            camelot_factory: Address::from_str("0x6EcCab422D763aC031210895C81787E87B43A652").expect("invalid Camelot factory"),
        }
    }
    
    /// Discover all pools for a token pair
    pub async fn discover_pools(&self, token_a: Address, token_b: Address) -> Vec<Pool> {
        let mut pools = Vec::new();
        
        // Sort tokens
        let (token0, token1) = if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        };
        
        // Find Uniswap V3 pools (all fee tiers)
        let v3_factory = UniswapV3Factory::new(self.uniswap_v3_factory, self.provider.clone());
        for fee in [100u32, 500, 3000, 10000] {
            if let Ok(pool_addr) = v3_factory.get_pool(token0, token1, fee.into()).call().await {
                if pool_addr != Address::zero() {
                    if let Some(pool) = self.fetch_v3_pool(pool_addr, fee).await {
                        pools.push(pool);
                    }
                }
            }
        }
        
        // Find SushiSwap pool
        if let Some(pool) = self.fetch_v2_pool(self.sushi_factory, token0, token1, PoolType::SushiSwap).await {
            pools.push(pool);
        }
        
        // Find Camelot pool
        if let Some(pool) = self.fetch_v2_pool(self.camelot_factory, token0, token1, PoolType::Camelot).await {
            pools.push(pool);
        }
        
        // Store pools
        let mut stored = self.pools.write().await;
        for pool in &pools {
            stored.insert(pool.address, pool.clone());
        }
        
        pools
    }
    
    async fn fetch_v3_pool(&self, pool_addr: Address, fee: u32) -> Option<Pool> {
        let pool = UniswapV3Pool::new(pool_addr, self.provider.clone());
        
        let token0 = pool.token_0().call().await
            .map_err(|e| warn!(pool = ?pool_addr, error = %e, "Failed to fetch token0")).ok()?;
        let token1 = pool.token_1().call().await
            .map_err(|e| warn!(pool = ?pool_addr, error = %e, "Failed to fetch token1")).ok()?;
        let liquidity = pool.liquidity().call().await
            .map_err(|e| warn!(pool = ?pool_addr, error = %e, "Failed to fetch liquidity")).ok()?;
        let slot0 = pool.slot_0().call().await
            .map_err(|e| warn!(pool = ?pool_addr, error = %e, "Failed to fetch slot0")).ok()?;
        
        // Skip pools with no liquidity
        if liquidity == 0 {
            return None;
        }
        
        // Convert sqrtPriceX96 to virtual reserves for V3
        // sqrtPriceX96 = sqrt(price) * 2^96
        // price = token1/token0 = reserve1/reserve0
        // We use virtual reserves based on liquidity and current price
        let sqrt_price_x96 = U256::from(slot0.0);
        let q96 = U256::from(1u128) << 96;
        
        // Virtual reserves at current tick:
        // reserve0 = L / sqrt(P)
        // reserve1 = L * sqrt(P)
        // Using fixed point math: L * 2^96 / sqrtPriceX96 and L * sqrtPriceX96 / 2^96
        let liq = U256::from(liquidity);
        
        // Prevent division by zero
        if sqrt_price_x96.is_zero() {
            return None;
        }
        
        let reserve0 = (liq * q96) / sqrt_price_x96;
        let reserve1 = (liq * sqrt_price_x96) / q96;
        
        // Skip if reserves too small
        if reserve0 < U256::from(1000u64) || reserve1 < U256::from(1000u64) {
            return None;
        }
        
        Some(Pool {
            address: pool_addr,
            token0,
            token1,
            pool_type: PoolType::UniswapV3 { fee },
            reserve0,
            reserve1,
            liquidity: liq,
            fee_bps: fee / 100, // Convert from 1/1000000 to bps
        })
    }
    
    async fn fetch_v2_pool(
        &self,
        factory: Address,
        token0: Address,
        token1: Address,
        pool_type: PoolType,
    ) -> Option<Pool> {
        let factory_contract = UniswapV2Factory::new(factory, self.provider.clone());
        let pair_addr = factory_contract.get_pair(token0, token1).call().await
            .map_err(|e| warn!(?token0, ?token1, error = %e, "Failed to fetch pair")).ok()?;
        
        if pair_addr == Address::zero() {
            return None;
        }
        
        let pair = UniswapV2Pair::new(pair_addr, self.provider.clone());
        let reserves = pair.get_reserves().call().await
            .map_err(|e| warn!(?pair_addr, error = %e, "Failed to fetch reserves")).ok()?;
        
        Some(Pool {
            address: pair_addr,
            token0,
            token1,
            pool_type,
            reserve0: U256::from(reserves.0),
            reserve1: U256::from(reserves.1),
            liquidity: U256::from(reserves.0) + U256::from(reserves.1),
            fee_bps: 30, // 0.30% for V2
        })
    }
    
    /// Update reserves for all stored pools - PARALLELIZED
    pub async fn refresh_all(&self) {
        use futures::future::join_all;
        
        let pools = self.pools.read().await;
        let pool_info: Vec<(Address, Pool)> = pools.iter()
            .map(|(addr, pool)| (*addr, pool.clone()))
            .collect();
        drop(pools);
        
        // Create parallel futures for each pool
        let futures: Vec<_> = pool_info.iter().map(|(addr, pool)| {
            let pool_addr = *addr;
            let pool = pool.clone();
            let provider = self.provider.clone();
            
            async move {
                let updated = match &pool.pool_type {
                    PoolType::UniswapV3 { fee: _ } => {
                        let v3_pool = UniswapV3Pool::new(pool_addr, provider.clone());
                        
                        // Fetch liquidity and slot0 sequentially (but many pools in parallel)
                        let liquidity = match v3_pool.liquidity().call().await {
                            Ok(l) => l,
                            Err(_) => return (pool_addr, None),
                        };
                        
                        if liquidity == 0 {
                            return (pool_addr, None);
                        }
                        
                        let slot0 = match v3_pool.slot_0().call().await {
                            Ok(s) => s,
                            Err(_) => return (pool_addr, None),
                        };
                        
                        let sqrt_price_x96 = U256::from(slot0.0);
                        let q96 = U256::from(1u128) << 96;
                        let liq = U256::from(liquidity);
                        
                        if sqrt_price_x96.is_zero() {
                            return (pool_addr, None);
                        }
                        
                        let reserve0 = (liq * q96) / sqrt_price_x96;
                        let reserve1 = (liq * sqrt_price_x96) / q96;
                        
                        if reserve0 >= U256::from(1000u64) && reserve1 >= U256::from(1000u64) {
                            Some(Pool {
                                reserve0,
                                reserve1,
                                liquidity: liq,
                                ..pool
                            })
                        } else {
                            None
                        }
                    }
                    PoolType::SushiSwap | PoolType::Camelot => {
                        let pair = UniswapV2Pair::new(pool_addr, provider);
                        if let Ok(reserves) = pair.get_reserves().call().await {
                            Some(Pool {
                                reserve0: U256::from(reserves.0),
                                reserve1: U256::from(reserves.1),
                                liquidity: U256::from(reserves.0) + U256::from(reserves.1),
                                ..pool
                            })
                        } else {
                            None
                        }
                    }
                };
                
                (pool_addr, updated)
            }
        }).collect();
        
        // Execute all in parallel
        let results = join_all(futures).await;
        
        // Update pools
        let mut pools = self.pools.write().await;
        for (addr, updated) in results {
            if let Some(p) = updated {
                pools.insert(addr, p);
            }
        }
    }
    
    /// Update reserves for a single pool
    pub async fn refresh_pool(&self, pool_addr: Address) {
        let pools = self.pools.read().await;
        let pool = match pools.get(&pool_addr) {
            Some(p) => p.clone(),
            None => return,
        };
        drop(pools);
        
        let updated = match &pool.pool_type {
            PoolType::UniswapV3 { fee } => {
                self.fetch_v3_pool(pool_addr, *fee).await
            }
            PoolType::SushiSwap | PoolType::Camelot => {
                let pair = UniswapV2Pair::new(pool_addr, self.provider.clone());
                if let Ok(reserves) = pair.get_reserves().call().await {
                    Some(Pool {
                        reserve0: U256::from(reserves.0),
                        reserve1: U256::from(reserves.1),
                        liquidity: U256::from(reserves.0) + U256::from(reserves.1),
                        ..pool
                    })
                } else {
                    None
                }
            }
        };
        
        if let Some(p) = updated {
            let mut pools = self.pools.write().await;
            pools.insert(pool_addr, p);
        }
    }
    
    /// Get all pools for a token pair
    pub async fn get_pools(&self, token0: Address, token1: Address) -> Vec<Pool> {
        let pools = self.pools.read().await;
        pools.values()
            .filter(|p| {
                (p.token0 == token0 && p.token1 == token1) ||
                (p.token0 == token1 && p.token1 == token0)
            })
            .cloned()
            .collect()
    }
}

/// Top tokens on Arbitrum for discovery - EXTENDED LIST
pub fn get_top_arbitrum_tokens() -> Vec<(&'static str, Address)> {
    use std::str::FromStr;
    
    vec![
        // === CORE TOKENS ===
        ("WETH", Address::from_str("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1").unwrap()),
        ("USDC", Address::from_str("0xaf88d065e77c8cC2239327C5EDb3A432268e5831").unwrap()),
        ("USDC.e", Address::from_str("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8").unwrap()),
        ("USDT", Address::from_str("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9").unwrap()),
        ("ARB", Address::from_str("0x912CE59144191C1204E64559FE8253a0e49E6548").unwrap()),
        ("WBTC", Address::from_str("0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f").unwrap()),
        ("DAI", Address::from_str("0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1").unwrap()),
        
        // === DEFI TOKENS (less efficient markets) ===
        ("GMX", Address::from_str("0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a").unwrap()),
        ("MAGIC", Address::from_str("0x539bdE0d7Dbd336b79148AA742883198BBF60342").unwrap()),
        ("RDNT", Address::from_str("0x3082CC23568eA640225c2467653dB90e9250AaA0").unwrap()),
        ("PENDLE", Address::from_str("0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8").unwrap()),
        ("GRAIL", Address::from_str("0x3d9907F9a368ad0a51Be60f7Da3b97cf940982D8").unwrap()),
        ("JONES", Address::from_str("0x10393c20975cF177a3513071bC110f7962CD67da").unwrap()),
        ("DPX", Address::from_str("0x6C2C06790b3E3E3c38e12Ee22F8183b37a13EE55").unwrap()),
        ("LINK", Address::from_str("0xf97f4df75117a78c1A5a0DBb814Af92458539FB4").unwrap()),
        ("UNI", Address::from_str("0xFa7F8980b0f1E64A2062791cc3b0871572f1F7f0").unwrap()),
        ("AAVE", Address::from_str("0xba5DdD1f9d7F570dc94a51479a000E3BCE967196").unwrap()),
        ("CRV", Address::from_str("0x11cDb42B0EB46D95f990BeDD4695A6e3fA034978").unwrap()),
        ("LDO", Address::from_str("0x13Ad51ed4F1B7e9Dc168d8a00cB3f4dDD85EfA60").unwrap()),
        ("wstETH", Address::from_str("0x5979D7b546E38E414F7E9822514be443A4800529").unwrap()),
        ("rETH", Address::from_str("0xEC70Dcb4A1EFa46b8F2D97C310C9c4790ba5ffA8").unwrap()),
        ("STG", Address::from_str("0x6694340fc020c5E6B96567843da2df01b2CE1eb6").unwrap()),
        ("VELA", Address::from_str("0x088cd8f5eF3652623c22D48b1605DCfE860Cd704").unwrap()),
        ("Y2K", Address::from_str("0x65c936f008BC34fE819bce9Fa5afD9dc2d49977f").unwrap()),
        ("WINR", Address::from_str("0xD77B108d4f6cefaa0Cae9506A934e825BEccA46e").unwrap()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::types::{Address, U256};
    use std::str::FromStr;

    fn weth() -> Address {
        Address::from_str("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1").unwrap()
    }
    fn usdc() -> Address {
        Address::from_str("0xaf88d065e77c8cC2239327C5EDb3A432268e5831").unwrap()
    }

    fn v2_pool(r0: u128, r1: u128, fee_bps: u32) -> Pool {
        Pool {
            address: Address::from_low_u64_be(1),
            token0: weth(),
            token1: usdc(),
            pool_type: PoolType::SushiSwap,
            reserve0: U256::from(r0),
            reserve1: U256::from(r1),
            liquidity: U256::from(r0) + U256::from(r1),
            fee_bps,
        }
    }

    // ── get_amount_out ──

    #[test]
    fn test_get_amount_out_basic() {
        // 5000 ETH (18 dec) / 10M USDC (6 dec), 0.3% fee
        let pool = v2_pool(
            5_000_000_000_000_000_000_000,
            10_000_000_000_000,
            30,
        );
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let out = pool.get_amount_out(amount_in, weth());
        // ~1994 USDC (slightly less than 2000 due to fee + impact)
        assert!(out > U256::from(1_990_000_000u64) && out < U256::from(2_000_000_000u64),
            "Expected ~1994 USDC, got {}", out);
    }

    #[test]
    fn test_get_amount_out_reverse_direction() {
        let pool = v2_pool(
            5_000_000_000_000_000_000_000,
            10_000_000_000_000,
            30,
        );
        let amount_in = U256::from(2_000_000_000u128); // 2000 USDC
        let out = pool.get_amount_out(amount_in, usdc());
        // Should get ~1 ETH back
        assert!(out > U256::from(990_000_000_000_000_000u128),
            "Expected ~1 ETH, got {}", out);
    }

    #[test]
    fn test_get_amount_out_zero_reserves() {
        let pool = v2_pool(0, 10_000_000_000_000, 30);
        let out = pool.get_amount_out(U256::from(1000u64), weth());
        assert_eq!(out, U256::zero());
    }

    #[test]
    fn test_get_amount_out_zero_input() {
        let pool = v2_pool(5_000_000_000_000_000_000_000, 10_000_000_000_000, 30);
        let out = pool.get_amount_out(U256::zero(), weth());
        assert_eq!(out, U256::zero());
    }

    #[test]
    fn test_get_amount_out_high_fee() {
        let pool = v2_pool(1_000_000, 1_000_000, 100); // 1% fee
        let out_high = pool.get_amount_out(U256::from(10_000u64), weth());

        let pool_low = v2_pool(1_000_000, 1_000_000, 10); // 0.1% fee
        let out_low = pool_low.get_amount_out(U256::from(10_000u64), weth());

        assert!(out_low > out_high, "lower fee should produce more output");
    }

    // ── get_price ──

    #[test]
    fn test_get_price_basic() {
        let pool = v2_pool(1_000_000, 2_000_000, 30);
        let price = pool.get_price();
        assert!((price - 2.0).abs() < 0.001, "price should be ~2.0, got {}", price);
    }

    #[test]
    fn test_get_price_zero_reserve() {
        let pool = v2_pool(0, 1_000_000, 30);
        assert_eq!(pool.get_price(), 0.0);
    }

    // ── Token list ──

    #[test]
    fn test_top_tokens_not_empty() {
        let tokens = get_top_arbitrum_tokens();
        assert!(tokens.len() >= 10);
    }

    #[test]
    fn test_top_tokens_weth_first() {
        let tokens = get_top_arbitrum_tokens();
        assert_eq!(tokens[0].0, "WETH");
        assert_eq!(tokens[0].1, weth());
    }

    #[test]
    fn test_top_tokens_no_duplicate_addresses() {
        let tokens = get_top_arbitrum_tokens();
        let mut addrs: Vec<_> = tokens.iter().map(|(_, a)| *a).collect();
        let before = addrs.len();
        addrs.sort();
        addrs.dedup();
        assert_eq!(addrs.len(), before, "duplicate addresses in token list");
    }
}
