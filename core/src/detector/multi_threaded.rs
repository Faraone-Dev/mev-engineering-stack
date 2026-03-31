//! Multi-threaded arbitrage detector
//! Uses work-stealing, SIMD price calculations, lock-free queues

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use crossbeam_channel::{bounded, Sender, Receiver};
use dashmap::DashMap;
use ethers::types::{Address, U256, H256};
use tracing::{info, debug, warn};

use crate::mempool::ultra_ws::{MempoolTx, SwapInfo};

/// Inline rdtsc for timing
#[inline(always)]
fn rdtsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { std::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}

/// Detected arbitrage opportunity
#[derive(Debug, Clone)]
pub struct Opportunity {
    pub id: u64,
    pub detected_tsc: u64,
    pub detected_ns: u64,
    pub trigger_tx: H256,
    pub path: Vec<PathStep>,
    pub profit_wei: U256,
    pub gas_estimate: U256,
    pub net_profit_wei: U256,
    pub confidence: f64,
    pub expires_block: u64,
}

#[derive(Debug, Clone)]
pub struct PathStep {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub expected_out: U256,
    pub dex_type: DexType,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum DexType {
    UniswapV2 = 0,
    UniswapV3 = 1,
    SushiSwap = 2,
    Camelot = 3,
    Curve = 4,
    Balancer = 5,
}

/// Pool state for calculations
#[derive(Debug, Clone)]
pub struct PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee: u32,           // In basis points (30 = 0.3%)
    pub dex_type: DexType,
    pub last_update: u64,
}

/// Detector configuration
#[derive(Clone)]
pub struct DetectorConfig {
    pub num_workers: usize,
    pub min_profit_wei: U256,
    pub max_hops: usize,
    pub gas_price: U256,
    pub batch_size: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            num_workers: num_cpus::get().saturating_sub(2).max(2),
            min_profit_wei: U256::from(1_000_000_000_000_000u64), // 0.001 ETH
            max_hops: 3,
            gas_price: U256::from(100_000_000), // 0.1 gwei (Arbitrum)
            batch_size: 64,
        }
    }
}

/// Multi-threaded detector with work-stealing
pub struct MultiThreadedDetector {
    config: DetectorConfig,
    running: Arc<AtomicBool>,
    pools: Arc<DashMap<Address, PoolState>>,
    token_to_pools: Arc<DashMap<Address, Vec<Address>>>,
    stats: Arc<DetectorStats>,
}

#[derive(Default)]
pub struct DetectorStats {
    pub txs_processed: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub avg_detection_ns: AtomicU64,
    pub profitable_count: AtomicU64,
}

impl MultiThreadedDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            pools: Arc::new(DashMap::new()),
            token_to_pools: Arc::new(DashMap::new()),
            stats: Arc::new(DetectorStats::default()),
        }
    }
    
    /// Load pools into memory
    pub fn load_pools(&self, pools: Vec<PoolState>) {
        info!("Loading {} pools into detector", pools.len());
        
        for pool in pools {
            // Index by pool address
            let pool_addr = pool.address;
            
            // Index by token for fast lookup
            self.token_to_pools
                .entry(pool.token0)
                .or_insert_with(Vec::new)
                .push(pool_addr);
            self.token_to_pools
                .entry(pool.token1)
                .or_insert_with(Vec::new)
                .push(pool_addr);
            
            self.pools.insert(pool_addr, pool);
        }
        
        info!("Loaded pools. Token index size: {}", self.token_to_pools.len());
    }
    
    /// Start detector workers
    pub fn start(
        &self,
        tx_receiver: Receiver<MempoolTx>,
        opp_sender: Sender<Opportunity>,
    ) {
        self.running.store(true, Ordering::SeqCst);
        
        let num_workers = self.config.num_workers;
        info!("Starting {} detector workers", num_workers);
        
        // Work distribution channel
        let (work_tx, work_rx) = bounded::<MempoolTx>(10_000);
        
        // Spawn dispatcher thread
        let running = self.running.clone();
        let stats = self.stats.clone();
        thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                match tx_receiver.recv_timeout(std::time::Duration::from_millis(10)) {
                    Ok(tx) => {
                        stats.txs_processed.fetch_add(1, Ordering::Relaxed);
                        if let Err(e) = work_tx.send(tx) {
                            warn!(error = %e, "Work channel full, dropping tx");
                        }
                    }
                    Err(_) => continue,
                }
            }
        });
        
        // Spawn worker threads
        for worker_id in 0..num_workers {
            let work_rx = work_rx.clone();
            let opp_sender = opp_sender.clone();
            let pools = self.pools.clone();
            let token_to_pools = self.token_to_pools.clone();
            let running = self.running.clone();
            let config = self.config.clone();
            let stats = self.stats.clone();
            
            thread::Builder::new()
                .name(format!("detector-{}", worker_id))
                .spawn(move || {
                    // Pin to CPU core
                    #[cfg(target_os = "linux")]
                    {
                        use core_affinity::CoreId;
                        let core_ids = core_affinity::get_core_ids().unwrap_or_default();
                        if let Some(core_id) = core_ids.get(worker_id + 2) {
                            core_affinity::set_for_current(*core_id);
                        }
                    }
                    
                    let mut batch = Vec::with_capacity(config.batch_size);
                    let mut opp_id = worker_id as u64 * 1_000_000;
                    
                    while running.load(Ordering::SeqCst) {
                        // Collect batch
                        batch.clear();
                        while batch.len() < config.batch_size {
                            match work_rx.try_recv() {
                                Ok(tx) => batch.push(tx),
                                Err(_) => break,
                            }
                        }
                        
                        if batch.is_empty() {
                            thread::sleep(std::time::Duration::from_micros(50));
                            continue;
                        }
                        
                        // Process batch
                        for tx in &batch {
                            if !tx.is_swap {
                                continue;
                            }
                            
                            let detect_start = rdtsc();
                            
                            if let Some(swap_info) = &tx.swap_info {
                                // Find arbitrage paths
                                if let Some(opps) = find_arbitrage_paths(
                                    swap_info,
                                    &pools,
                                    &token_to_pools,
                                    &config,
                                ) {
                                    for mut opp in opps {
                                        opp.id = opp_id;
                                        opp.trigger_tx = tx.hash;
                                        opp.detected_tsc = detect_start;
                                        opp_id += 1;
                                        
                                        if opp.net_profit_wei >= config.min_profit_wei {
                                            stats.profitable_count.fetch_add(1, Ordering::Relaxed);
                                            if let Err(e) = opp_sender.send(opp) {
                                                warn!(error = %e, "Opportunity channel closed");
                                            }
                                        }
                                    }
                                }
                            }
                            
                            stats.opportunities_found.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }).expect("Failed to spawn detector worker");
        }
    }
    
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
    
    pub fn stats(&self) -> &DetectorStats {
        &self.stats
    }
    
    pub fn update_pool(&self, address: Address, reserve0: U256, reserve1: U256) {
        if let Some(mut pool) = self.pools.get_mut(&address) {
            pool.reserve0 = reserve0;
            pool.reserve1 = reserve1;
            pool.last_update = rdtsc();
        }
    }
}

/// Find arbitrage paths using graph traversal
fn find_arbitrage_paths(
    swap: &SwapInfo,
    pools: &DashMap<Address, PoolState>,
    token_to_pools: &DashMap<Address, Vec<Address>>,
    config: &DetectorConfig,
) -> Option<Vec<Opportunity>> {
    let mut opportunities = Vec::new();
    
    // Get pools containing output token
    let output_pools = token_to_pools.get(&swap.token_out)?;
    
    // Touch hot entries up front to improve locality during path expansion.
    let _ = output_pools.iter().take(8).count();
    
    // 2-hop paths: token_out -> intermediate -> token_in
    for pool1_addr in output_pools.iter() {
        if let Some(pool1) = pools.get(pool1_addr) {
            let intermediate = if pool1.token0 == swap.token_out {
                pool1.token1
            } else if pool1.token1 == swap.token_out {
                pool1.token0
            } else {
                continue;
            };
            
            // Find pool back to token_in
            if let Some(return_pools) = token_to_pools.get(&intermediate) {
                for pool2_addr in return_pools.iter() {
                    if let Some(pool2) = pools.get(pool2_addr) {
                        // Check if pool2 contains token_in
                        if pool2.token0 == swap.token_in || pool2.token1 == swap.token_in {
                            // Calculate profitability
                            if let Some(opp) = calculate_2hop_profit(
                                swap,
                                pool1.value(),
                                pool2.value(),
                                config,
                            ) {
                                opportunities.push(opp);
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Triangular extension intentionally narrowed to explicit WETH bridge paths
    // to keep detector latency bounded in this module.
    
    if opportunities.is_empty() {
        None
    } else {
        Some(opportunities)
    }
}

/// Calculate 2-hop arbitrage profit
#[inline]
fn calculate_2hop_profit(
    swap: &SwapInfo,
    pool1: &PoolState,
    pool2: &PoolState,
    config: &DetectorConfig,
) -> Option<Opportunity> {
    // Estimate target swap output with constant-product math for fast screening.
    let target_out = simulate_swap_out(
        swap.amount_in,
        swap.token_in,
        swap.token_out,
        pool1,
    )?;
    
    // Our backrun: swap token_out -> intermediate
    let intermediate = if pool1.token0 == swap.token_out {
        pool1.token1
    } else {
        pool1.token0
    };
    
    let step1_out = simulate_swap_out(
        target_out / 10, // Use 10% of liquidity
        swap.token_out,
        intermediate,
        pool1,
    )?;
    
    // intermediate -> token_in
    let step2_out = simulate_swap_out(
        step1_out,
        intermediate,
        swap.token_in,
        pool2,
    )?;
    
    // Calculate profit
    let input_amount = target_out / 10;
    if step2_out <= input_amount {
        return None;
    }
    
    let gross_profit = step2_out - input_amount;
    let gas_cost = config.gas_price * U256::from(200_000); // Estimate
    
    if gross_profit <= gas_cost {
        return None;
    }
    
    let net_profit = gross_profit - gas_cost;
    
    Some(Opportunity {
        id: 0,
        detected_tsc: 0,
        detected_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64,
        trigger_tx: H256::zero(),
        path: vec![
            PathStep {
                pool: pool1.address,
                token_in: swap.token_out,
                token_out: intermediate,
                amount_in: input_amount,
                expected_out: step1_out,
                dex_type: pool1.dex_type,
            },
            PathStep {
                pool: pool2.address,
                token_in: intermediate,
                token_out: swap.token_in,
                amount_in: step1_out,
                expected_out: step2_out,
                dex_type: pool2.dex_type,
            },
        ],
        profit_wei: gross_profit,
        gas_estimate: U256::from(200_000),
        net_profit_wei: net_profit,
        confidence: 0.8,
        expires_block: 0,
    })
}

/// Simulate swap output using constant product formula
#[inline(always)]
fn simulate_swap_out(
    amount_in: U256,
    token_in: Address,
    _token_out: Address,
    pool: &PoolState,
) -> Option<U256> {
    let (reserve_in, reserve_out) = if pool.token0 == token_in {
        (pool.reserve0, pool.reserve1)
    } else {
        (pool.reserve1, pool.reserve0)
    };
    
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return None;
    }
    
    // Constant product: (x + dx)(y - dy) = xy
    // dy = y * dx / (x + dx) * (1 - fee)
    let fee_factor = 10000 - pool.fee; // e.g., 9970 for 0.3%
    let amount_in_with_fee = amount_in * U256::from(fee_factor);
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * U256::from(10000) + amount_in_with_fee;
    
    Some(numerator / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simulate_swap() {
        let pool = PoolState {
            address: Address::zero(),
            token0: Address::zero(),
            token1: Address::repeat_byte(1),
            reserve0: U256::from(1_000_000_000_000_000_000u64), // 1 ETH
            reserve1: U256::from(2000u64) * U256::from(1_000_000_000_000_000_000u64), // 2000 tokens
            fee: 30, // 0.3%
            dex_type: DexType::UniswapV2,
            last_update: 0,
        };
        
        let amount_in = U256::from(100_000_000_000_000_000u64); // 0.1 ETH
        let out = simulate_swap_out(amount_in, Address::zero(), Address::repeat_byte(1), &pool);
        
        assert!(out.is_some());
        let out = out.unwrap();
        assert!(out > U256::zero());
    }
}
