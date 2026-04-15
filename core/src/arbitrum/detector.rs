// Arbitrage Detection for Arbitrum
// Finds profitable opportunities across DEXes

use super::pools::{Pool, PoolManager, PoolType};
use ethers::types::{Address, U256};
use std::sync::Arc;

/// Arbitrage opportunity
#[derive(Clone, Debug)]
pub struct ArbitrageOpportunity {
    pub path: Vec<ArbitrageStep>,
    pub input_token: Address,
    pub input_amount: U256,
    pub output_amount: U256,
    pub profit: U256,
    pub profit_bps: u32,
    pub gas_estimate: U256,
    pub net_profit: U256,
}

#[derive(Clone, Debug)]
pub struct ArbitrageStep {
    pub pool: Address,
    pub pool_type: PoolType,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

/// Arbitrage detector
pub struct ArbitrageDetector {
    pool_manager: Arc<PoolManager>,
    min_profit_bps: u32,      // Minimum profit in basis points
    gas_price_wei: U256,       // Current gas price
}

impl ArbitrageDetector {
    pub fn new(pool_manager: Arc<PoolManager>, min_profit_bps: u32) -> Self {
        Self {
            pool_manager,
            min_profit_bps,
            gas_price_wei: U256::from(100_000_000), // 0.1 gwei default for Arbitrum
        }
    }
    
    pub fn set_gas_price(&mut self, gas_price: U256) {
        self.gas_price_wei = gas_price;
    }
    
    /// Find 2-hop arbitrage: token -> intermediate -> token
    /// Example: WETH -> USDC (pool A) -> WETH (pool B)
    pub async fn find_two_hop_arb(
        &self,
        token: Address,
        intermediate: Address,
        amount: U256,
    ) -> Option<ArbitrageOpportunity> {
        // Get all pools for first leg
        let pools_first = self.pool_manager.get_pools(token, intermediate).await;
        // Get all pools for second leg
        let pools_second = self.pool_manager.get_pools(intermediate, token).await;
        
        if pools_first.is_empty() || pools_second.is_empty() {
            return None;
        }
        
        let mut best_opportunity: Option<ArbitrageOpportunity> = None;
        let min_profit_bps = self.min_profit_bps;
        
        // Try all combinations
        for pool_a in &pools_first {
            for pool_b in &pools_second {
                // Skip same pool
                if pool_a.address == pool_b.address {
                    continue;
                }
                
                // Calculate output from first swap
                let amount_mid = pool_a.get_amount_out(amount, token);
                if amount_mid.is_zero() {
                    continue;
                }
                
                // Calculate output from second swap
                let amount_out = pool_b.get_amount_out(amount_mid, intermediate);
                if amount_out.is_zero() {
                    continue;
                }
                
                // Check profitability
                if amount_out <= amount {
                    continue;
                }
                
                let profit = amount_out - amount;
                let profit_bps_u256 = profit * U256::from(10000u32) / amount;
                let profit_bps = profit_bps_u256.low_u32();
                
                if profit_bps < min_profit_bps {
                    continue;
                }
                
                // Estimate gas cost
                let gas_estimate = self.estimate_gas(&pool_a.pool_type, &pool_b.pool_type);
                let gas_cost = gas_estimate * self.gas_price_wei;
                
                // Net profit
                let net_profit = if profit > gas_cost {
                    profit - gas_cost
                } else {
                    continue; // Not profitable after gas
                };
                
                let opportunity = ArbitrageOpportunity {
                    path: vec![
                        ArbitrageStep {
                            pool: pool_a.address,
                            pool_type: pool_a.pool_type.clone(),
                            token_in: token,
                            token_out: intermediate,
                            amount_in: amount,
                            amount_out: amount_mid,
                        },
                        ArbitrageStep {
                            pool: pool_b.address,
                            pool_type: pool_b.pool_type.clone(),
                            token_in: intermediate,
                            token_out: token,
                            amount_in: amount_mid,
                            amount_out: amount_out,
                        },
                    ],
                    input_token: token,
                    input_amount: amount,
                    output_amount: amount_out,
                    profit,
                    profit_bps,
                    gas_estimate,
                    net_profit,
                };
                
                // Keep best opportunity
                if let Some(ref best) = best_opportunity {
                    if net_profit > best.net_profit {
                        best_opportunity = Some(opportunity);
                    }
                } else {
                    best_opportunity = Some(opportunity);
                }
            }
        }
        
        best_opportunity
    }
    
    /// Find 3-hop triangular arbitrage
    /// Example: WETH -> USDC -> ARB -> WETH
    pub async fn find_triangular_arb(
        &self,
        token_a: Address, // Start/end token (usually WETH)
        token_b: Address, // Intermediate 1
        token_c: Address, // Intermediate 2
        amount: U256,
    ) -> Option<ArbitrageOpportunity> {
        // Get pools for each leg
        let pools_ab = self.pool_manager.get_pools(token_a, token_b).await;
        let pools_bc = self.pool_manager.get_pools(token_b, token_c).await;
        let pools_ca = self.pool_manager.get_pools(token_c, token_a).await;
        
        if pools_ab.is_empty() || pools_bc.is_empty() || pools_ca.is_empty() {
            return None;
        }
        
        let mut best_opportunity: Option<ArbitrageOpportunity> = None;
        
        // Try all combinations (limited to avoid explosion)
        for pool_ab in pools_ab.iter().take(3) {
            for pool_bc in pools_bc.iter().take(3) {
                for pool_ca in pools_ca.iter().take(3) {
                    // Calculate path
                    let amount_b = pool_ab.get_amount_out(amount, token_a);
                    if amount_b.is_zero() {
                        continue;
                    }
                    
                    let amount_c = pool_bc.get_amount_out(amount_b, token_b);
                    if amount_c.is_zero() {
                        continue;
                    }
                    
                    let amount_out = pool_ca.get_amount_out(amount_c, token_c);
                    if amount_out.is_zero() || amount_out <= amount {
                        continue;
                    }
                    
                    let profit = amount_out - amount;
                    let profit_bps_u256 = profit * U256::from(10000u32) / amount;
                    let profit_bps = profit_bps_u256.low_u32();
                    
                    if profit_bps < self.min_profit_bps {
                        continue;
                    }
                    
                    // Gas estimate for 3 swaps
                    let gas_estimate = U256::from(400_000); // ~400k gas for 3 swaps
                    let gas_cost = gas_estimate * self.gas_price_wei;
                    
                    let net_profit = if profit > gas_cost {
                        profit - gas_cost
                    } else {
                        continue;
                    };
                    
                    let opportunity = ArbitrageOpportunity {
                        path: vec![
                            ArbitrageStep {
                                pool: pool_ab.address,
                                pool_type: pool_ab.pool_type.clone(),
                                token_in: token_a,
                                token_out: token_b,
                                amount_in: amount,
                                amount_out: amount_b,
                            },
                            ArbitrageStep {
                                pool: pool_bc.address,
                                pool_type: pool_bc.pool_type.clone(),
                                token_in: token_b,
                                token_out: token_c,
                                amount_in: amount_b,
                                amount_out: amount_c,
                            },
                            ArbitrageStep {
                                pool: pool_ca.address,
                                pool_type: pool_ca.pool_type.clone(),
                                token_in: token_c,
                                token_out: token_a,
                                amount_in: amount_c,
                                amount_out: amount_out,
                            },
                        ],
                        input_token: token_a,
                        input_amount: amount,
                        output_amount: amount_out,
                        profit,
                        profit_bps,
                        gas_estimate,
                        net_profit,
                    };
                    
                    if let Some(ref best) = best_opportunity {
                        if net_profit > best.net_profit {
                            best_opportunity = Some(opportunity);
                        }
                    } else {
                        best_opportunity = Some(opportunity);
                    }
                }
            }
        }
        
        best_opportunity
    }
    
    /// Scan all common pairs for arbitrage - WITH DEBUG
    pub async fn scan_all(&self, amount: U256) -> Vec<ArbitrageOpportunity> {
        use super::pools::get_top_arbitrum_tokens;
        
        let tokens = get_top_arbitrum_tokens();
        let mut opportunities = Vec::new();
        
        // WETH is always the base
        let weth = tokens[0].1;
        
        // Two-hop: WETH -> X -> WETH
        for (_name, token) in tokens.iter().skip(1) {
            if let Some(opp) = self.find_two_hop_arb(weth, *token, amount).await {
                opportunities.push(opp);
            }
        }
        
        // Also try with debug for best near-miss
        if opportunities.is_empty() {
            // Find best near-miss opportunity
            let mut best_ratio = 0.0f64;
            let mut best_pair = String::new();
            
            for (_name, token) in tokens.iter().skip(1) {
                let pools_first = self.pool_manager.get_pools(weth, *token).await;
                let pools_second = self.pool_manager.get_pools(*token, weth).await;
                
                for pool_a in &pools_first {
                    for pool_b in &pools_second {
                        if pool_a.address == pool_b.address {
                            continue;
                        }
                        
                        let amount_mid = pool_a.get_amount_out(amount, weth);
                        if amount_mid.is_zero() {
                            continue;
                        }
                        
                        let amount_out = pool_b.get_amount_out(amount_mid, *token);
                        if amount_out.is_zero() {
                            continue;
                        }
                        
                        let ratio = amount_out.as_u128() as f64 / amount.as_u128() as f64;
                        if ratio > best_ratio {
                            best_ratio = ratio;
                            best_pair = _name.to_string();
                        }
                    }
                }
            }
            
            // Log best near-miss every 100 scans (controlled externally)
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count % 50 == 0 && best_ratio > 0.0 {
                let bps = ((best_ratio - 1.0) * 10000.0) as i32;
                println!("    📊 Best ratio: {:.6} ({}bps) on WETH/{}", best_ratio, bps, best_pair);
            }
        }
        
        // Triangular: WETH -> X -> Y -> WETH
        for i in 1..tokens.len() {
            for j in (i + 1)..tokens.len() {
                if let Some(opp) = self.find_triangular_arb(
                    weth,
                    tokens[i].1,
                    tokens[j].1,
                    amount,
                ).await {
                    opportunities.push(opp);
                }
            }
        }
        
        // Sort by profit
        opportunities.sort_by(|a, b| b.net_profit.cmp(&a.net_profit));
        
        opportunities
    }
    
    fn estimate_gas(&self, pool_a: &PoolType, pool_b: &PoolType) -> U256 {
        let gas_a = match pool_a {
            PoolType::UniswapV3 { .. } => 150_000,
            PoolType::SushiSwap | PoolType::Camelot => 100_000,
        };
        
        let gas_b = match pool_b {
            PoolType::UniswapV3 { .. } => 150_000,
            PoolType::SushiSwap | PoolType::Camelot => 100_000,
        };
        
        // Flash loan overhead + swaps
        U256::from(50_000 + gas_a + gas_b)
    }
}

/// Calculate optimal input amount using binary search
pub fn find_optimal_amount(
    pool_a: &Pool,
    pool_b: &Pool,
    token: Address,
    intermediate: Address,
) -> U256 {
    let mut low = U256::from(1_000_000_000_000_000u64); // 0.001 ETH
    let mut high = pool_a.reserve0.min(pool_a.reserve1) / 10; // Max 10% of pool
    
    let mut best_amount = low;
    let mut best_profit = U256::zero();
    
    // Binary search for optimal
    for _ in 0..64 {
        if low >= high {
            break;
        }
        
        let mid = (low + high) / 2;
        
        // Calculate profit at mid
        let amount_mid = pool_a.get_amount_out(mid, token);
        let amount_out = pool_b.get_amount_out(amount_mid, intermediate);
        
        let profit = if amount_out > mid {
            amount_out - mid
        } else {
            U256::zero()
        };
        
        if profit > best_profit {
            best_profit = profit;
            best_amount = mid;
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    
    best_amount
}
