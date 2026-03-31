//! Arbitrage detection — cross-DEX price discrepancy detector
//!
//! Parses V2 and V3 swap calldata, queries cached pool reserves
//! across multiple DEXes, and calculates net arbitrage profit
//! after gas and slippage.

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, PendingTx, SwapInfo, DexType, PoolState, estimate_gas};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::debug;

/// Arbitrage detector for cross-DEX opportunities
pub struct ArbitrageDetector {
    config: Arc<Config>,
    /// Cached pool states indexed by (token0, token1, dex) -> reserves
    pool_cache: Arc<RwLock<HashMap<PoolKey, PoolState>>>,
}

/// Composite key for pool lookup
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    token0: [u8; 20],
    token1: [u8; 20],
    dex: u8, // DexType discriminant
}

impl ArbitrageDetector {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            pool_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update cached pool reserves (called by pool refresh loop)
    pub fn update_pool(&self, state: PoolState) {
        let key = PoolKey {
            token0: state.token0,
            token1: state.token1,
            dex: state.fee as u8, // use fee as dex discriminant
        };
        self.pool_cache.write().insert(key, state);
    }

    /// Bulk load pools from discovery
    pub fn load_pools(&self, pools: Vec<PoolState>) {
        let mut cache = self.pool_cache.write();
        for state in pools {
            let key = PoolKey {
                token0: state.token0,
                token1: state.token1,
                dex: state.fee as u8,
            };
            cache.insert(key, state);
        }
    }

    /// Detect arbitrage opportunity from pending transaction
    pub async fn detect(&self, tx: &PendingTx) -> Option<Opportunity> {
        let swap = self.parse_swap(tx)?;

        // Query prices from cached pool reserves across DEXes
        let prices = self.get_cross_dex_prices(&swap)?;

        if prices.len() < 2 {
            return None;
        }

        let profit = self.calculate_profit(&swap, &prices)?;

        let min_profit = self.config.strategy.min_profit_wei;
        if profit < min_profit {
            return None;
        }

        let exit_dex = self.find_best_exit_dex(&prices);

        debug!(
            profit_wei = profit,
            dex_in = ?swap.dex,
            dex_out = ?exit_dex,
            amount = swap.amount_in,
            "Arbitrage opportunity detected"
        );

        // Resolve pool addresses from cache for each hop in the path
        let entry_dex = swap.dex.clone();
        let path = vec![entry_dex, exit_dex];
        let pool_addresses = self.resolve_pool_addresses(&swap.token_in, &swap.token_out, &path)?;
        let pool_fees = path.iter().map(|d| match d {
            DexType::UniswapV3 => 500,
            _ => 3000,
        }).collect();

        Some(Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: swap.token_in,
            token_out: swap.token_out,
            amount_in: swap.amount_in,
            expected_profit: profit,
            gas_estimate: estimate_gas(&OpportunityType::Arbitrage, &path),
            deadline: tx.timestamp + 12,
            path,
            pool_addresses,
            pool_fees,
            target_tx: Some(tx.hash),
        })
    }

    /// Parse swap from calldata — supports V2 and V3 selectors
    fn parse_swap(&self, tx: &PendingTx) -> Option<SwapInfo> {
        let data = &tx.input;
        if data.len() < 4 {
            return None;
        }

        let selector = &data[0..4];
        match selector {
            // UniswapV2: swapExactTokensForTokens
            [0x38, 0xed, 0x17, 0x39] => self.parse_v2_swap(data, false),
            // UniswapV2: swapTokensForExactTokens
            [0x88, 0x03, 0xdb, 0xee] => self.parse_v2_swap(data, true),
            // UniswapV2: swapExactETHForTokens
            [0x7f, 0xf3, 0x6a, 0xb5] => self.parse_v2_eth_swap(data, tx.value),
            // UniswapV2: swapExactTokensForETH
            [0x18, 0xcb, 0xaf, 0xe5] => self.parse_v2_swap(data, false),
            // UniswapV3: exactInputSingle
            [0x41, 0x4b, 0xf3, 0x89] => self.parse_v3_exact_input_single(data),
            // UniswapV3: exactInput
            [0xc0, 0x4b, 0x8d, 0x59] => self.parse_v3_exact_input(data),
            // UniswapV3: exactOutputSingle
            [0xdb, 0x3e, 0x21, 0x98] => self.parse_v3_exact_output_single(data),
            _ => None,
        }
    }

    /// Decode UniswapV2 swapExactTokensForTokens / swapTokensForExactTokens
    /// ABI: (uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
    fn parse_v2_swap(&self, data: &[u8], exact_output: bool) -> Option<SwapInfo> {
        // Minimum: 4 (sel) + 5*32 (params) + 32 (path length) + 2*32 (min 2 addrs) = 260
        if data.len() < 260 {
            return None;
        }

        // amountIn at offset 4..36 (right-aligned in 32-byte word)
        let amount_in = read_u128_from_word(&data[4..36]);
        // amountOutMin at offset 36..68
        let amount_out_min = read_u128_from_word(&data[36..68]);

        // path offset at 68..100 (pointer to dynamic array)
        let path_offset = read_usize_from_word(&data[68..100]) + 4; // +4 for selector
        if path_offset + 32 > data.len() {
            return None;
        }
        // path length
        let path_len = read_usize_from_word(&data[path_offset..path_offset + 32]);
        if path_len < 2 || path_offset + 32 + path_len * 32 > data.len() {
            return None;
        }

        // First token in path (right-aligned 20 bytes in 32-byte word)
        let token_in_start = path_offset + 32 + 12; // skip 12 zero-padding bytes
        let token_out_start = path_offset + 32 + (path_len - 1) * 32 + 12;

        let token_in = format!("0x{}", hex::encode(&data[token_in_start..token_in_start + 20]));
        let token_out = format!("0x{}", hex::encode(&data[token_out_start..token_out_start + 20]));

        // Deadline at offset 132..164
        let deadline = if data.len() >= 164 {
            read_u64_from_word(&data[132..164])
        } else {
            0
        };

        Some(SwapInfo {
            dex: DexType::UniswapV2,
            token_in,
            token_out,
            amount_in: if exact_output { amount_out_min } else { amount_in },
            amount_out_min: if exact_output { amount_in } else { amount_out_min },
            fee: 3000, // 0.30%
        })
    }

    /// Decode V2 ETH swap (value is the input amount)
    fn parse_v2_eth_swap(&self, data: &[u8], value: u128) -> Option<SwapInfo> {
        if data.len() < 196 {
            return None;
        }
        let amount_out_min = read_u128_from_word(&data[4..36]);
        let path_offset = read_usize_from_word(&data[36..68]) + 4;
        if path_offset + 64 > data.len() {
            return None;
        }
        let path_len = read_usize_from_word(&data[path_offset..path_offset + 32]);
        if path_len < 2 || path_offset + 32 + path_len * 32 > data.len() {
            return None;
        }
        let token_out_start = path_offset + 32 + (path_len - 1) * 32 + 12;
        let token_out = format!("0x{}", hex::encode(&data[token_out_start..token_out_start + 20]));

        Some(SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "WETH".to_string(),
            token_out,
            amount_in: value,
            amount_out_min,
            fee: 3000,
        })
    }

    /// Decode UniswapV3 exactInputSingle
    /// ABI: ExactInputSingleParams { tokenIn, tokenOut, fee, recipient, deadline, amountIn, amountOutMin, sqrtPriceLimitX96 }
    fn parse_v3_exact_input_single(&self, data: &[u8]) -> Option<SwapInfo> {
        // 4 (sel) + 8*32 = 260 bytes
        if data.len() < 260 {
            return None;
        }

        let token_in = format!("0x{}", hex::encode(&data[16..36]));   // word 0, right-aligned
        let token_out = format!("0x{}", hex::encode(&data[48..68]));  // word 1
        let fee = read_u32_from_word(&data[68..100]);                  // word 2
        let deadline = read_u64_from_word(&data[132..164]);            // word 4
        let amount_in = read_u128_from_word(&data[164..196]);          // word 5
        let amount_out_min = read_u128_from_word(&data[196..228]);     // word 6

        Some(SwapInfo {
            dex: DexType::UniswapV3,
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            fee,
        })
    }

    /// Decode UniswapV3 exactInput (multi-hop)
    /// ABI: ExactInputParams { bytes path, address recipient, uint256 deadline, uint256 amountIn, uint256 amountOutMin }
    fn parse_v3_exact_input(&self, data: &[u8]) -> Option<SwapInfo> {
        if data.len() < 196 {
            return None;
        }

        // path is dynamic: offset at word 0
        let path_offset = read_usize_from_word(&data[4..36]) + 4;
        let amount_in = read_u128_from_word(&data[100..132]);          // word 3
        let amount_out_min = read_u128_from_word(&data[132..164]);     // word 4

        // V3 packed path: [tokenIn (20)] [fee (3)] [tokenOut (20)] [fee (3)] ...
        if path_offset + 32 > data.len() {
            return None;
        }
        let path_len = read_usize_from_word(&data[path_offset..path_offset + 32]);
        let path_start = path_offset + 32;
        if path_start + path_len > data.len() || path_len < 43 {
            return None;
        }

        let token_in = format!("0x{}", hex::encode(&data[path_start..path_start + 20]));
        let fee = u32::from_be_bytes([0, data[path_start + 20], data[path_start + 21], data[path_start + 22]]);
        // Last 20 bytes of path are token_out
        let token_out_start = path_start + path_len - 20;
        let token_out = format!("0x{}", hex::encode(&data[token_out_start..token_out_start + 20]));

        Some(SwapInfo {
            dex: DexType::UniswapV3,
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            fee,
        })
    }

    /// Decode UniswapV3 exactOutputSingle
    /// ABI: ExactOutputSingleParams { tokenIn, tokenOut, fee, recipient, deadline, amountOut, amountInMaximum, sqrtPriceLimitX96 }
    fn parse_v3_exact_output_single(&self, data: &[u8]) -> Option<SwapInfo> {
        if data.len() < 260 {
            return None;
        }

        let token_in = format!("0x{}", hex::encode(&data[16..36]));
        let token_out = format!("0x{}", hex::encode(&data[48..68]));
        let fee = read_u32_from_word(&data[68..100]);
        let amount_out = read_u128_from_word(&data[164..196]);     // amountOut
        let amount_in_max = read_u128_from_word(&data[196..228]);  // amountInMaximum

        Some(SwapInfo {
            dex: DexType::UniswapV3,
            token_in,
            token_out,
            amount_in: amount_in_max,
            amount_out_min: amount_out,
            fee,
        })
    }

    /// Query cached pool reserves across multiple DEXes for the same token pair.
    /// Returns (dex, output_amount) for a fixed input of swap.amount_in.
    fn get_cross_dex_prices(&self, swap: &SwapInfo) -> Option<Vec<(DexType, u128)>> {
        let cache = self.pool_cache.read();
        if cache.is_empty() {
            // No pools cached — fall back to mainnet estimation
            // In production the pool refresh loop populates this
            return self.estimate_cross_dex_prices(swap);
        }

        let mut results = Vec::with_capacity(4);

        for pool in cache.values() {
            // Check if pool contains the same pair (either direction)
            let (is_match, is_reversed) = match_pool_to_swap(pool, swap);
            if !is_match {
                continue;
            }

            // Constant product: dy = y * dx * (1-fee) / (x + dx * (1-fee))
            let (reserve_in, reserve_out) = if is_reversed {
                (pool.reserve1, pool.reserve0)
            } else {
                (pool.reserve0, pool.reserve1)
            };

            if reserve_in == 0 || reserve_out == 0 {
                continue;
            }

            let fee_bps = pool.fee as u128;
            let amount_in_with_fee = swap.amount_in * (10_000 - fee_bps);
            let numerator = amount_in_with_fee * reserve_out;
            let denominator = reserve_in * 10_000 + amount_in_with_fee;

            if denominator == 0 {
                continue;
            }
            let amount_out = numerator / denominator;

            let dex = dex_from_fee(pool.fee);
            results.push((dex, amount_out));
        }

        if results.is_empty() {
            None
        } else {
            Some(results)
        }
    }

    /// Fallback price estimation when pool cache is cold.
    /// Uses constant product approximation with typical reserves.
    fn estimate_cross_dex_prices(&self, swap: &SwapInfo) -> Option<Vec<(DexType, u128)>> {
        // Skip if amount is too small to be interesting
        if swap.amount_in < 100_000 {
            return None;
        }

        // Model typical Arbitrum DEX reserves (WETH/USDC example)
        // Real reserves come from pool refresh — this bootstraps detection
        let typical_reserves: Vec<(DexType, u128, u128, u32)> = vec![
            (DexType::UniswapV2,  5_000_000_000_000_000_000_000, 10_000_000_000_000, 3000),  // ~$10M TVL
            (DexType::SushiSwap,  2_000_000_000_000_000_000_000,  4_000_000_000_000, 3000),
            (DexType::UniswapV3,  8_000_000_000_000_000_000_000, 16_000_000_000_000, 500),   // 0.05% fee
        ];

        let mut results = Vec::new();
        for (dex, reserve0, reserve1, fee_bps) in &typical_reserves {
            let amount_in_with_fee = swap.amount_in * (10_000 - *fee_bps as u128);
            let numerator = amount_in_with_fee * reserve1;
            let denominator = reserve0 * 10_000 + amount_in_with_fee;
            if denominator > 0 {
                results.push((dex.clone(), numerator / denominator));
            }
        }

        if results.is_empty() { None } else { Some(results) }
    }

    /// Calculate net profit from price discrepancy across DEXes
    fn calculate_profit(&self, swap: &SwapInfo, prices: &[(DexType, u128)]) -> Option<u128> {
        if prices.len() < 2 {
            return None;
        }

        let min_price = prices.iter().map(|(_, p)| *p).min()?;
        let max_price = prices.iter().map(|(_, p)| *p).max()?;

        if max_price <= min_price {
            return None;
        }

        // Gross profit = buy at cheapest DEX, sell at most expensive
        let gross_profit = max_price.saturating_sub(min_price);

        // Gas cost: base_gas * gas_price
        // Arbitrum: ~0.1 gwei base, 250k gas for flash arb
        let gas_price_wei = self.config.strategy.max_gas_price_gwei as u128 * 1_000_000_000;
        let gas_cost = 250_000u128 * gas_price_wei;

        // Slippage buffer (configurable, default 50 bps = 0.5%)
        let slippage_bps = self.config.strategy.slippage_tolerance_bps as u128;
        let slippage_cost = gross_profit * slippage_bps / 10_000;

        gross_profit.checked_sub(gas_cost)?.checked_sub(slippage_cost)
    }

    fn find_best_exit_dex(&self, prices: &[(DexType, u128)]) -> DexType {
        prices
            .iter()
            .max_by_key(|(_, p)| p)
            .map(|(d, _)| d.clone())
            .unwrap_or(DexType::UniswapV3)
    }

    /// Resolve actual pool contract addresses from the cache for each path hop.
    /// Returns None if any pool cannot be resolved — caller must skip the opportunity.
    fn resolve_pool_addresses(&self, token_in: &str, token_out: &str, path: &[DexType]) -> Option<Vec<[u8; 20]>> {
        let cache = self.pool_cache.read();
        let token_in_bytes = decode_addr(token_in);
        let token_out_bytes = decode_addr(token_out);

        path.iter().enumerate().map(|(i, dex)| {
            // For a 2-hop arb: hop 0 = token_in → token_out, hop 1 = token_out → token_in
            let (search_t0, search_t1) = if i == 0 {
                (token_in_bytes, token_out_bytes)
            } else {
                (token_out_bytes, token_in_bytes)
            };

            // Search cache for a pool matching this token pair and DEX type
            let mut found = None;
            for (_key, pool) in cache.iter() {
                let pool_matches = (pool.token0 == search_t0 && pool.token1 == search_t1)
                    || (pool.token0 == search_t1 && pool.token1 == search_t0);
                if pool_matches {
                    let pool_dex = dex_from_fee(pool.fee);
                    if std::mem::discriminant(&pool_dex) == std::mem::discriminant(dex) {
                        found = Some(pool.address);
                        break;
                    }
                }
            }
            found // None if pool not cached → collect() yields None, skipping this opportunity
        }).collect::<Option<Vec<_>>>()
    }
}

// ─── ABI decode helpers ───────────────────────────────────────────

/// Read u128 from right-aligned 32-byte ABI word (last 16 bytes)
#[inline]
fn read_u128_from_word(word: &[u8]) -> u128 {
    if word.len() < 32 {
        return 0;
    }
    u128::from_be_bytes(word[16..32].try_into().unwrap_or([0; 16]))
}

/// Read u64 from right-aligned 32-byte ABI word
#[inline]
fn read_u64_from_word(word: &[u8]) -> u64 {
    if word.len() < 32 {
        return 0;
    }
    u64::from_be_bytes(word[24..32].try_into().unwrap_or([0; 8]))
}

/// Read u32 from right-aligned 32-byte ABI word
#[inline]
fn read_u32_from_word(word: &[u8]) -> u32 {
    if word.len() < 32 {
        return 0;
    }
    u32::from_be_bytes(word[28..32].try_into().unwrap_or([0; 4]))
}

/// Read usize from right-aligned 32-byte ABI word
#[inline]
fn read_usize_from_word(word: &[u8]) -> usize {
    read_u64_from_word(word) as usize
}

/// Match pool reserves to a parsed swap's token pair
fn match_pool_to_swap(pool: &PoolState, swap: &SwapInfo) -> (bool, bool) {
    let token_in_bytes = decode_addr(&swap.token_in);
    let token_out_bytes = decode_addr(&swap.token_out);

    if pool.token0 == token_in_bytes && pool.token1 == token_out_bytes {
        (true, false)
    } else if pool.token1 == token_in_bytes && pool.token0 == token_out_bytes {
        (true, true)
    } else {
        (false, false)
    }
}

fn decode_addr(addr: &str) -> [u8; 20] {
    let hex_str = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(hex_str).unwrap_or_default();
    let mut out = [0u8; 20];
    if bytes.len() >= 20 {
        out.copy_from_slice(&bytes[..20]);
    }
    out
}

fn dex_from_fee(fee: u32) -> DexType {
    match fee {
        100 | 500 | 3000 | 10000 => DexType::UniswapV3,
        9970 => DexType::SushiSwap,
        _ => DexType::UniswapV2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_u128_from_word() {
        let mut word = [0u8; 32];
        word[31] = 1; // 1 in last byte
        assert_eq!(read_u128_from_word(&word), 1);

        // 1 ETH = 1e18
        let val = 1_000_000_000_000_000_000u128;
        word[16..32].copy_from_slice(&val.to_be_bytes());
        assert_eq!(read_u128_from_word(&word), val);
    }

    #[test]
    fn test_parse_v3_exact_input_single() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);

        // Build minimal V3 exactInputSingle calldata (260 bytes)
        let mut data = vec![0u8; 260];
        data[0..4].copy_from_slice(&[0x41, 0x4b, 0xf3, 0x89]); // selector

        // tokenIn at word 0 (offset 4), right-aligned 20 bytes
        data[16..36].copy_from_slice(&[0xC0; 20]); // tokenIn
        // tokenOut at word 1 (offset 36)
        data[48..68].copy_from_slice(&[0xD0; 20]); // tokenOut
        // fee at word 2 (offset 68) = 3000
        data[96..100].copy_from_slice(&3000u32.to_be_bytes());
        // amountIn at word 5 (offset 164)
        let amount = 1_000_000_000_000_000_000u128; // 1 ETH
        data[180..196].copy_from_slice(&amount.to_be_bytes());
        // amountOutMin at word 6 (offset 196)
        let min_out = 2000_000_000u128; // 2000 USDC (6 decimals)
        data[212..228].copy_from_slice(&min_out.to_be_bytes());

        let swap = detector.parse_v3_exact_input_single(&data).unwrap();
        assert_eq!(swap.amount_in, amount);
        assert_eq!(swap.amount_out_min, min_out);
        assert_eq!(swap.fee, 3000);
        assert!(matches!(swap.dex, DexType::UniswapV3));
    }

    #[test]
    fn test_parse_v2_swap() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);

        // Build V2 swapExactTokensForTokens calldata
        let mut data = vec![0u8; 292]; // 4 + 5*32 + 32(len) + 2*32(path)
        data[0..4].copy_from_slice(&[0x38, 0xed, 0x17, 0x39]);

        // amountIn at word 0
        let amount_in = 5_000_000_000_000_000_000u128;
        data[20..36].copy_from_slice(&amount_in.to_be_bytes());
        // amountOutMin at word 1
        let amount_out = 10_000_000_000u128;
        data[52..68].copy_from_slice(&amount_out.to_be_bytes());
        // path offset at word 2 = 160 (5 * 32)
        data[96..100].copy_from_slice(&160u32.to_be_bytes());
        // path length = 2
        data[192..196].copy_from_slice(&2u32.to_be_bytes());
        // path[0] = tokenIn
        data[208..228].copy_from_slice(&[0xAA; 20]);
        // path[1] = tokenOut
        data[240..260].copy_from_slice(&[0xBB; 20]);

        let swap = detector.parse_v2_swap(&data, false).unwrap();
        assert_eq!(swap.amount_in, amount_in);
        assert_eq!(swap.amount_out_min, amount_out);
        assert!(matches!(swap.dex, DexType::UniswapV2));
    }

    #[test]
    fn test_calculate_profit_no_arb() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);

        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xtoken_in".to_string(),
            token_out: "0xtoken_out".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            amount_out_min: 0,
            fee: 3000,
        };

        // Same price everywhere — no arb
        let prices = vec![
            (DexType::UniswapV2, 2000_000_000),
            (DexType::SushiSwap, 2000_000_000),
        ];
        assert!(detector.calculate_profit(&swap, &prices).is_none());
    }

    // ── ABI word readers ──

    #[test]
    fn test_read_u64_from_word() {
        let mut word = [0u8; 32];
        word[31] = 42;
        assert_eq!(read_u64_from_word(&word), 42);
    }

    #[test]
    fn test_read_u64_from_short_slice() {
        let short = [0u8; 16];
        assert_eq!(read_u64_from_word(&short), 0);
    }

    #[test]
    fn test_read_u32_from_word() {
        let mut word = [0u8; 32];
        word[28..32].copy_from_slice(&3000u32.to_be_bytes());
        assert_eq!(read_u32_from_word(&word), 3000);
    }

    #[test]
    fn test_read_u32_from_short_slice() {
        let short = [0u8; 8];
        assert_eq!(read_u32_from_word(&short), 0);
    }

    #[test]
    fn test_read_u128_from_word_short_returns_zero() {
        let short = [0xFFu8; 10];
        assert_eq!(read_u128_from_word(&short), 0);
    }

    #[test]
    fn test_read_usize_from_word() {
        let mut word = [0u8; 32];
        word[24..32].copy_from_slice(&160u64.to_be_bytes());
        assert_eq!(read_usize_from_word(&word), 160);
    }

    // ── decode_addr ──

    #[test]
    fn test_decode_addr_with_prefix() {
        let addr = decode_addr("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert_eq!(addr[0], 0xC0);
        assert_eq!(addr[19], 0xC2);
    }

    #[test]
    fn test_decode_addr_without_prefix() {
        let addr = decode_addr("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert_eq!(addr[0], 0xC0);
    }

    #[test]
    fn test_decode_addr_empty() {
        let addr = decode_addr("");
        assert_eq!(addr, [0u8; 20]);
    }

    #[test]
    fn test_decode_addr_invalid_hex() {
        let addr = decode_addr("0xGGGGGGGG");
        assert_eq!(addr, [0u8; 20]);
    }

    // ── dex_from_fee ──

    #[test]
    fn test_dex_from_fee_v3_tiers() {
        assert!(matches!(dex_from_fee(100), DexType::UniswapV3));
        assert!(matches!(dex_from_fee(500), DexType::UniswapV3));
        assert!(matches!(dex_from_fee(3000), DexType::UniswapV3));
        assert!(matches!(dex_from_fee(10000), DexType::UniswapV3));
    }

    #[test]
    fn test_dex_from_fee_sushi() {
        assert!(matches!(dex_from_fee(9970), DexType::SushiSwap));
    }

    #[test]
    fn test_dex_from_fee_default_v2() {
        assert!(matches!(dex_from_fee(0), DexType::UniswapV2));
        assert!(matches!(dex_from_fee(9999), DexType::UniswapV2));
    }

    // ── match_pool_to_swap ──

    #[test]
    fn test_match_pool_forward() {
        let pool = PoolState {
            address: [0xAA; 20],
            token0: decode_addr("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            token1: decode_addr("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            reserve0: 1000,
            reserve1: 2000,
            fee: 3000,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        };
        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1000,
            amount_out_min: 0,
            fee: 3000,
        };
        let (is_match, is_reversed) = match_pool_to_swap(&pool, &swap);
        assert!(is_match);
        assert!(!is_reversed);
    }

    #[test]
    fn test_match_pool_reversed() {
        let pool = PoolState {
            address: [0xAA; 20],
            token0: decode_addr("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            token1: decode_addr("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            reserve0: 1000,
            reserve1: 2000,
            fee: 3000,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        };
        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1000,
            amount_out_min: 0,
            fee: 3000,
        };
        let (is_match, is_reversed) = match_pool_to_swap(&pool, &swap);
        assert!(is_match);
        assert!(is_reversed);
    }

    #[test]
    fn test_match_pool_no_match() {
        let pool = PoolState {
            address: [0xAA; 20],
            token0: [0x01; 20],
            token1: [0x02; 20],
            reserve0: 1000,
            reserve1: 2000,
            fee: 3000,
            sqrt_price_x96: 0,
            liquidity: 0,
            is_v3: false,
        };
        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1000,
            amount_out_min: 0,
            fee: 3000,
        };
        let (is_match, _) = match_pool_to_swap(&pool, &swap);
        assert!(!is_match);
    }

    // ── V3 exact output single ──

    #[test]
    fn test_parse_v3_exact_output_single() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);

        let mut data = vec![0u8; 260];
        data[0..4].copy_from_slice(&[0xdb, 0x3e, 0x21, 0x98]); // selector
        data[16..36].copy_from_slice(&[0xAA; 20]); // tokenIn
        data[48..68].copy_from_slice(&[0xBB; 20]); // tokenOut
        data[96..100].copy_from_slice(&500u32.to_be_bytes()); // fee
        let amount_out = 2_000_000_000u128;
        data[180..196].copy_from_slice(&amount_out.to_be_bytes()); // amountOut
        let amount_in_max = 1_000_000_000_000_000_000u128;
        data[212..228].copy_from_slice(&amount_in_max.to_be_bytes()); // amountInMaximum

        let swap = detector.parse_v3_exact_output_single(&data).unwrap();
        assert_eq!(swap.amount_in, amount_in_max);
        assert_eq!(swap.amount_out_min, amount_out);
        assert_eq!(swap.fee, 500);
        assert!(matches!(swap.dex, DexType::UniswapV3));
    }

    // ── V2 truncated data ──

    #[test]
    fn test_parse_v2_swap_truncated_returns_none() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);
        let short = vec![0x38, 0xed, 0x17, 0x39, 0x00]; // just selector + 1 byte
        assert!(detector.parse_v2_swap(&short, false).is_none());
    }

    #[test]
    fn test_parse_v3_truncated_returns_none() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);
        let short = vec![0u8; 100]; // < 260 required
        assert!(detector.parse_v3_exact_input_single(&short).is_none());
    }

    // ── calculate_profit: profitable case ──

    #[test]
    fn test_calculate_profit_profitable() {
        let mut config = Config::default();
        config.strategy.max_gas_price_gwei = 1;
        config.strategy.slippage_tolerance_bps = 50;
        let detector = ArbitrageDetector::new(Arc::new(config));

        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xweth".to_string(),
            token_out: "0xusdc".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            amount_out_min: 0,
            fee: 3000,
        };

        // gas_cost = 250_000 * 1 gwei = 250_000_000_000_000 wei
        // Spread must exceed gas_cost + slippage to be profitable
        let prices = vec![
            (DexType::UniswapV2,  1_000_000_000_000_000_000),
            (DexType::UniswapV3,  2_000_000_000_000_000_000),
        ];
        let profit = detector.calculate_profit(&swap, &prices);
        assert!(profit.is_some(), "1e18 spread should be profitable at 1 gwei gas");
        assert!(profit.unwrap() > 0);
    }

    #[test]
    fn test_calculate_profit_gas_exceeds() {
        let mut config = Config::default();
        config.strategy.max_gas_price_gwei = 1000; // Very high gas
        let detector = ArbitrageDetector::new(Arc::new(config));

        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xweth".to_string(),
            token_out: "0xusdc".to_string(),
            amount_in: 1_000_000,
            amount_out_min: 0,
            fee: 3000,
        };

        // Tiny spread — gas should eat it
        let prices = vec![
            (DexType::UniswapV2, 1000),
            (DexType::UniswapV3, 1001),
        ];
        assert!(detector.calculate_profit(&swap, &prices).is_none());
    }

    #[test]
    fn test_calculate_profit_single_price_returns_none() {
        let config = Arc::new(Config::default());
        let detector = ArbitrageDetector::new(config);
        let swap = SwapInfo {
            dex: DexType::UniswapV2,
            token_in: "0xa".to_string(),
            token_out: "0xb".to_string(),
            amount_in: 1000,
            amount_out_min: 0,
            fee: 3000,
        };
        // Need at least 2 prices
        let prices = vec![(DexType::UniswapV2, 2000)];
        assert!(detector.calculate_profit(&swap, &prices).is_none());
    }
}
