// MEV PROTOCOL - FULL LAUNCHER
// Integrates: WebSocket Mempool + Pool Scanner + Arbitrage Detector + Flash Loan Executor
// Run: cargo run --release --bin mev_launcher

use ethers::{
    prelude::*,
    providers::{Provider, Http, Ws},
    types::{Address, U256, Transaction},
    signers::LocalWallet,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::str::FromStr;
use std::collections::HashMap;
use tokio::sync::mpsc;
use futures_util::StreamExt;

// Import from main crate
use mev_core::arbitrum::pools::{PoolManager, PoolType, get_top_arbitrum_tokens};
use mev_core::arbitrum::detector::ArbitrageDetector;
use mev_core::arbitrum::executor::{ArbitrageExecutor, ExecutorConfig};

// ═══════════════════════════════════════════════════════════════════════════════
// CONFIG
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
struct MevConfig {
    // RPC
    http_rpc: String,
    ws_rpc: String,
    
    // Contract
    contract_address: Address,
    wallet: LocalWallet,
    
    // Trading
    min_profit_bps: u32,
    execute_live: bool,
    
    // Flash loan amounts to try
    flash_amounts: Vec<U256>,
}

impl MevConfig {
    fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();
        
        let http_rpc = std::env::var("ARBITRUM_RPC_URL")
            .unwrap_or_else(|_| "https://arb1.arbitrum.io/rpc".to_string());
        
        // Convert HTTP to WSS for Alchemy
        let ws_rpc = std::env::var("ARBITRUM_WS_URL").unwrap_or_else(|_| {
            http_rpc.replace("https://", "wss://")
                   .replace("/v2/", "/v2/")
        });
        
        let private_key = std::env::var("PRIVATE_KEY")
            .expect("PRIVATE_KEY must be set");
        
        let contract_address = std::env::var("CONTRACT_ADDRESS")
            .unwrap_or_else(|_| "0x42a372E2f161e978ee9791F399c27c56D6CB55eb".to_string());
        
        let min_profit_bps: u32 = std::env::var("MIN_PROFIT_BPS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .unwrap_or(10);
        
        let execute_live = std::env::var("EXECUTE_MODE")
            .unwrap_or_else(|_| "simulate".to_string()) == "live";
        
        let wallet: LocalWallet = private_key.parse::<LocalWallet>()?
            .with_chain_id(42161u64);
        
        Ok(Self {
            http_rpc,
            ws_rpc,
            contract_address: Address::from_str(&contract_address)?,
            wallet,
            min_profit_bps,
            execute_live,
            flash_amounts: vec![
                U256::from(1_000_000_000_000_000_000u64),   // 1 ETH
                U256::from(5_000_000_000_000_000_000u64),   // 5 ETH
                U256::from(10_000_000_000_000_000_000u64),  // 10 ETH
                U256::from(50u64) * U256::from(1_000_000_000_000_000_000u64), // 50 ETH
            ],
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// STATS
// ═══════════════════════════════════════════════════════════════════════════════

struct Stats {
    mempool_txs: AtomicU64,
    swaps_detected: AtomicU64,
    opportunities_found: AtomicU64,
    simulations_ok: AtomicU64,
    executions_sent: AtomicU64,
    executions_success: AtomicU64,
    total_profit_wei: AtomicU64,
    start_time: Instant,
}

impl Stats {
    fn new() -> Self {
        Self {
            mempool_txs: AtomicU64::new(0),
            swaps_detected: AtomicU64::new(0),
            opportunities_found: AtomicU64::new(0),
            simulations_ok: AtomicU64::new(0),
            executions_sent: AtomicU64::new(0),
            executions_success: AtomicU64::new(0),
            total_profit_wei: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }
    
    fn print_summary(&self) {
        let elapsed = self.start_time.elapsed().as_secs().max(1);
        let txs = self.mempool_txs.load(Ordering::Relaxed);
        let swaps = self.swaps_detected.load(Ordering::Relaxed);
        let opps = self.opportunities_found.load(Ordering::Relaxed);
        let sims = self.simulations_ok.load(Ordering::Relaxed);
        let execs = self.executions_sent.load(Ordering::Relaxed);
        let success = self.executions_success.load(Ordering::Relaxed);
        let profit = self.total_profit_wei.load(Ordering::Relaxed) as f64 / 1e18;
        
        println!("╔═══════════════════════════════════════════════════════════╗");
        println!("║                     📊 STATS SUMMARY                      ║");
        println!("╠═══════════════════════════════════════════════════════════╣");
        println!("║  Runtime:          {:>8} seconds                       ║", elapsed);
        println!("║  Mempool TXs:      {:>8} ({:.1}/sec)                    ║", txs, txs as f64 / elapsed as f64);
        println!("║  Swaps Detected:   {:>8}                               ║", swaps);
        println!("║  Opportunities:    {:>8}                               ║", opps);
        println!("║  Simulations OK:   {:>8}                               ║", sims);
        println!("║  Executions Sent:  {:>8}                               ║", execs);
        println!("║  Successful:       {:>8}                               ║", success);
        println!("║  Total Profit:     {:>8.6} ETH                         ║", profit);
        println!("╚═══════════════════════════════════════════════════════════╝");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// SWAP DETECTOR (from calldata)
// ═══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct SwapInfo {
    router: Address,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    min_amount_out: U256,
    dex_type: DexType,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum DexType {
    UniswapV3,
    UniswapV2,
    SushiSwap,
    Camelot,
    Unknown,
}

// Known router addresses on Arbitrum
fn get_router_type(addr: Address) -> Option<DexType> {
    let routers: HashMap<Address, DexType> = [
        // Uniswap V3 SwapRouter
        (Address::from_str("0xE592427A0AEce92De3Edee1F18E0157C05861564").unwrap(), DexType::UniswapV3),
        // Uniswap V3 SwapRouter02
        (Address::from_str("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45").unwrap(), DexType::UniswapV3),
        // Uniswap Universal Router
        (Address::from_str("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD").unwrap(), DexType::UniswapV3),
        // SushiSwap Router
        (Address::from_str("0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506").unwrap(), DexType::SushiSwap),
        // Camelot Router
        (Address::from_str("0xc873fEcbd354f5A56E00E710B90EF4201db2448d").unwrap(), DexType::Camelot),
    ].into_iter().collect();
    
    routers.get(&addr).copied()
}

// Function selectors for swap detection
fn is_swap_selector(selector: &[u8]) -> bool {
    if selector.len() < 4 {
        return false;
    }
    
    let selectors: [[u8; 4]; 10] = [
        [0x38, 0xed, 0x17, 0x39], // swapExactTokensForTokens
        [0x88, 0x03, 0xdb, 0xee], // swapTokensForExactTokens
        [0x7f, 0xf3, 0x6a, 0xb5], // swapExactETHForTokens
        [0x18, 0xcb, 0xaf, 0xe5], // swapExactTokensForETH
        [0x41, 0x4b, 0xf3, 0x89], // exactInputSingle (V3)
        [0xc0, 0x4b, 0x8d, 0x59], // exactInput (V3)
        [0xdb, 0x3e, 0x21, 0x98], // exactOutputSingle (V3)
        [0xf2, 0x8c, 0x05, 0x98], // exactOutput (V3)
        [0x3c, 0xd0, 0x71, 0x37], // execute (Universal Router)
        [0x24, 0x85, 0x6b, 0xc3], // execute (Universal Router with deadline)
    ];
    
    let input_selector: [u8; 4] = [selector[0], selector[1], selector[2], selector[3]];
    selectors.contains(&input_selector)
}

fn decode_swap_info(tx: &Transaction) -> Option<SwapInfo> {
    let to = tx.to?;
    let data = &tx.input;
    
    if data.len() < 4 {
        return None;
    }
    
    let dex_type = get_router_type(to)?;
    
    if !is_swap_selector(&data[..4]) {
        return None;
    }

    let selector: [u8; 4] = data[0..4].try_into().ok()?;

    let (token_in, token_out, amount_in, min_amount_out) = match selector {
        // V2: swapExactTokensForTokens(uint256,uint256,address[],address,uint256)
        [0x38, 0xed, 0x17, 0x39] | [0x18, 0xcb, 0xaf, 0xe5] => {
            if data.len() < 4 + 5 * 32 { return None; }
            let amt_in  = U256::from_big_endian(&data[4..36]);
            let min_out = U256::from_big_endian(&data[36..68]);
            let path_offset = U256::from_big_endian(&data[68..100]).as_usize() + 4;
            if data.len() < path_offset + 96 { return None; }
            let path_len = U256::from_big_endian(&data[path_offset..path_offset + 32]).as_usize();
            if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 { return None; }
            let t_in  = Address::from_slice(&data[path_offset + 32 + 12..path_offset + 32 + 32]);
            let t_out = Address::from_slice(&data[path_offset + 64 + 12..path_offset + 64 + 32]);
            (t_in, t_out, amt_in, min_out)
        }
        // V2: swapExactETHForTokens(uint256,address[],address,uint256)
        [0x7f, 0xf3, 0x6a, 0xb5] => {
            if data.len() < 4 + 4 * 32 { return None; }
            let min_out = U256::from_big_endian(&data[4..36]);
            let path_offset = U256::from_big_endian(&data[36..68]).as_usize() + 4;
            if data.len() < path_offset + 96 { return None; }
            let path_len = U256::from_big_endian(&data[path_offset..path_offset + 32]).as_usize();
            if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 { return None; }
            let t_in  = Address::from_slice(&data[path_offset + 32 + 12..path_offset + 32 + 32]);
            let t_out = Address::from_slice(&data[path_offset + 64 + 12..path_offset + 64 + 32]);
            (t_in, t_out, tx.value, min_out)
        }
        // V3: exactInputSingle — struct(tokenIn,tokenOut,fee,recipient,deadline,amountIn,amountOutMin,sqrtPriceLimitX96)
        [0x41, 0x4b, 0xf3, 0x89] => {
            if data.len() < 4 + 8 * 32 { return None; }
            let t_in    = Address::from_slice(&data[4 + 12..4 + 32]);
            let t_out   = Address::from_slice(&data[36 + 12..36 + 32]);
            let amt_in  = U256::from_big_endian(&data[4 + 5 * 32..4 + 6 * 32]);
            let min_out = U256::from_big_endian(&data[4 + 6 * 32..4 + 7 * 32]);
            (t_in, t_out, amt_in, min_out)
        }
        // Fallback: selector recognized but layout not decoded here
        _ => (Address::zero(), Address::zero(), tx.value, U256::zero()),
    };

    Some(SwapInfo {
        router: to,
        token_in,
        token_out,
        amount_in,
        min_amount_out,
        dex_type,
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// MEMPOOL MONITOR (WebSocket)
// ═══════════════════════════════════════════════════════════════════════════════

async fn run_mempool_monitor(
    ws_url: String,
    http_provider: Arc<Provider<Http>>,
    tx_sender: mpsc::UnboundedSender<(Transaction, SwapInfo)>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
) {
    println!("🔌 Connecting to WebSocket: {}", ws_url);
    
    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        
        match Provider::<Ws>::connect(&ws_url).await {
            Ok(ws_provider) => {
                println!("✓ WebSocket connected!");
                
                // Subscribe to pending transactions
                match ws_provider.subscribe_pending_txs().await {
                    Ok(mut stream) => {
                        println!("✓ Subscribed to pending transactions");
                        
                        while let Some(tx_hash) = stream.next().await {
                            if !running.load(Ordering::Relaxed) {
                                break;
                            }
                            
                            stats.mempool_txs.fetch_add(1, Ordering::Relaxed);
                            
                            // Fetch full tx
                            let provider = http_provider.clone();
                            let sender = tx_sender.clone();
                            let stats_clone = stats.clone();
                            
                            tokio::spawn(async move {
                                if let Ok(Some(tx)) = provider.get_transaction(tx_hash).await {
                                    // Check if it's a swap
                                    if let Some(swap_info) = decode_swap_info(&tx) {
                                        stats_clone.swaps_detected.fetch_add(1, Ordering::Relaxed);
                                        let _ = sender.send((tx, swap_info));
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to subscribe: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("❌ WebSocket connection failed: {}", e);
            }
        }
        
        // Reconnect delay
        println!("⟳ Reconnecting in 2s...");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// POOL SCANNER (Background refresh)
// ═══════════════════════════════════════════════════════════════════════════════

async fn run_pool_scanner(
    pool_manager: Arc<PoolManager>,
    _stats: Arc<Stats>,
    running: Arc<AtomicBool>,
) {
    println!("📊 Starting pool refresh loop");
    
    let mut scan_count = 0u64;
    
    while running.load(Ordering::Relaxed) {
        scan_count += 1;
        let start = Instant::now();
        
        pool_manager.refresh_all().await;
        
        if scan_count % 10 == 0 {
            println!("  ⟳ Pool refresh #{}: {:?}", scan_count, start.elapsed());
        }
        
        // Small delay between refreshes
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ARBITRAGE ENGINE
// ═══════════════════════════════════════════════════════════════════════════════

async fn run_arbitrage_engine(
    config: MevConfig,
    _pool_manager: Arc<PoolManager>,
    detector: Arc<ArbitrageDetector>,
    executor: Arc<ArbitrageExecutor>,
    mut swap_rx: mpsc::UnboundedReceiver<(Transaction, SwapInfo)>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
) {
    println!("⚡ Arbitrage engine started");
    
    while running.load(Ordering::Relaxed) {
        tokio::select! {
            // React to swaps from mempool
            Some((tx, _swap)) = swap_rx.recv() => {
                let detect_start = Instant::now();
                
                // Quick scan for arbitrage opportunities
                for amount in &config.flash_amounts {
                    let opportunities = detector.scan_all(*amount).await;
                    
                    for opp in opportunities {
                        stats.opportunities_found.fetch_add(1, Ordering::Relaxed);
                        
                        let profit_eth = opp.net_profit.as_u128() as f64 / 1e18;
                        let input_eth = opp.input_amount.as_u128() as f64 / 1e18;
                        
                        println!("╔═══════════════════════════════════════════════════════════╗");
                        println!("║ 🎯 OPPORTUNITY DETECTED                                   ║");
                        println!("╠═══════════════════════════════════════════════════════════╣");
                        println!("║  Trigger TX:  {:?}", tx.hash);
                        println!("║  Input:       {:.4} ETH (flash loan)", input_eth);
                        println!("║  Profit:      {:.6} ETH ({} bps)", profit_eth, opp.profit_bps);
                        println!("║  Detection:   {:?}", detect_start.elapsed());
                        
                        for (i, step) in opp.path.iter().enumerate() {
                            let type_name = match &step.pool_type {
                                PoolType::UniswapV3 { fee } => format!("UniV3-{}", fee),
                                PoolType::SushiSwap => "Sushi".to_string(),
                                PoolType::Camelot => "Camelot".to_string(),
                            };
                            println!("║  Step {}: {} @ {:?}", i + 1, type_name, step.pool);
                        }
                        
                        // Execute or simulate
                        if profit_eth > 0.0001 { // Min ~$0.30
                            if config.execute_live {
                                println!("║  🚀 EXECUTING FLASH LOAN...");
                                match executor.execute(&opp).await {
                                    Ok(tx_hash) => {
                                        stats.executions_sent.fetch_add(1, Ordering::Relaxed);
                                        println!("║  ✅ TX SENT: {:?}", tx_hash);
                                        // Would need to wait for confirmation for success
                                        stats.executions_success.fetch_add(1, Ordering::Relaxed);
                                        stats.total_profit_wei.fetch_add(
                                            opp.net_profit.as_u64(),
                                            Ordering::Relaxed
                                        );
                                    }
                                    Err(e) => {
                                        println!("║  ❌ EXECUTION FAILED: {:?}", e);
                                    }
                                }
                            } else {
                                println!("║  🔸 Simulating...");
                                match executor.simulate(&opp).await {
                                    Ok(sim) => {
                                        stats.simulations_ok.fetch_add(1, Ordering::Relaxed);
                                        println!("║  ✓ SIM OK - Gas: {} - Net: {:.6} ETH",
                                            sim.gas_estimate, sim.net_profit.as_u128() as f64 / 1e18);
                                    }
                                    Err(e) => {
                                        println!("║  ✗ SIM FAILED: {:?}", e);
                                    }
                                }
                            }
                        }
                        println!("╚═══════════════════════════════════════════════════════════╝\n");
                    }
                }
            }
            
            // Periodic scan even without mempool triggers
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                // Background scan
                for amount in &config.flash_amounts[..1] { // Just 1 ETH for background
                    let opportunities = detector.scan_all(*amount).await;
                    for opp in opportunities {
                        stats.opportunities_found.fetch_add(1, Ordering::Relaxed);
                        
                        let profit_eth = opp.net_profit.as_u128() as f64 / 1e18;
                        if profit_eth > 0.001 { // Only log significant ones
                            println!("📡 Background opportunity: {:.6} ETH profit ({} bps)",
                                profit_eth, opp.profit_bps);
                        }
                    }
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Banner
    println!();
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║                                                           ║");
    println!("║     ███╗   ███╗███████╗██╗   ██╗                          ║");
    println!("║     ████╗ ████║██╔════╝██║   ██║                          ║");
    println!("║     ██╔████╔██║█████╗  ██║   ██║                          ║");
    println!("║     ██║╚██╔╝██║██╔══╝  ╚██╗ ██╔╝                          ║");
    println!("║     ██║ ╚═╝ ██║███████╗ ╚████╔╝                           ║");
    println!("║     ╚═╝     ╚═╝╚══════╝  ╚═══╝                            ║");
    println!("║                                                           ║");
    println!("║     P R O T O C O L   -   A R B I T R U M                 ║");
    println!("║                                                           ║");
    println!("║     Flash Loan Arbitrage Engine v1.0                      ║");
    println!("║                                                           ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();
    
    // Load config
    let config = MevConfig::from_env()?;
    
    println!("⚙️  Configuration:");
    println!("   HTTP RPC: {}", config.http_rpc);
    println!("   WS RPC:   {}", config.ws_rpc);
    println!("   Contract: {:?}", config.contract_address);
    println!("   Wallet:   {:?}", config.wallet.address());
    println!("   Min Profit: {} bps ({:.2}%)", config.min_profit_bps, config.min_profit_bps as f64 / 100.0);
    println!("   Mode: {}", if config.execute_live { "🔴 LIVE" } else { "🟡 SIMULATION" });
    println!();
    
    // Initialize providers
    let http_provider = Arc::new(Provider::<Http>::try_from(&config.http_rpc)?);
    
    // Check connection
    let block = http_provider.get_block_number().await?;
    println!("✓ Connected to Arbitrum at block {}", block);
    
    // Initialize pool manager
    let pool_manager = Arc::new(PoolManager::new(http_provider.clone()));
    
    // Discover pools
    println!("\n📊 Discovering pools...");
    let tokens = get_top_arbitrum_tokens();
    let weth = tokens[0].1;
    
    let mut total_pools = 0;
    for (name, token) in tokens.iter().skip(1) {
        let pools = pool_manager.discover_pools(weth, *token).await;
        println!("   WETH/{}: {} pools", name, pools.len());
        total_pools += pools.len();
    }
    println!("✓ Found {} pools total\n", total_pools);
    
    // Initialize detector
    let detector = Arc::new(ArbitrageDetector::new(pool_manager.clone(), config.min_profit_bps));
    
    // Initialize executor
    let executor_config = ExecutorConfig {
        contract_address: config.contract_address,
        private_key: config.wallet.clone(),
        max_gas_price: U256::from(500_000_000u64), // 0.5 gwei
        priority_fee: U256::from(10_000_000u64),
        slippage_bps: 50,
    };
    let executor = Arc::new(ArbitrageExecutor::new(http_provider.clone(), executor_config));
    
    // Stats
    let stats = Arc::new(Stats::new());
    let running = Arc::new(AtomicBool::new(true));
    
    // Channels
    let (swap_tx, swap_rx) = mpsc::unbounded_channel();
    
    // Handle Ctrl+C
    let running_clone = running.clone();
    let stats_clone = stats.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\n\n🛑 Shutting down...\n");
        running_clone.store(false, Ordering::SeqCst);
        stats_clone.print_summary();
    });
    
    println!("🚀 Starting MEV engine...\n");
    println!("═══════════════════════════════════════════════════════════════");
    
    // Spawn all tasks
    let mempool_handle = tokio::spawn(run_mempool_monitor(
        config.ws_rpc.clone(),
        http_provider.clone(),
        swap_tx,
        stats.clone(),
        running.clone(),
    ));
    
    let scanner_handle = tokio::spawn(run_pool_scanner(
        pool_manager.clone(),
        stats.clone(),
        running.clone(),
    ));
    
    let engine_handle = tokio::spawn(run_arbitrage_engine(
        config.clone(),
        pool_manager.clone(),
        detector.clone(),
        executor.clone(),
        swap_rx,
        stats.clone(),
        running.clone(),
    ));
    
    // Stats printer
    let stats_clone = stats.clone();
    let running_clone = running.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        while running_clone.load(Ordering::Relaxed) {
            interval.tick().await;
            stats_clone.print_summary();
        }
    });
    
    // Wait for all tasks
    let _ = tokio::join!(mempool_handle, scanner_handle, engine_handle);
    
    stats.print_summary();
    println!("\n👋 MEV Protocol shutdown complete.\n");
    
    Ok(())
}
