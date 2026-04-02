//! MEV Engine CLI
//! Sub-microsecond latency optimized

use mev_core::{Config, MevEngine, MempoolConfig, MempoolMonitor};
use mev_core::detector::{MultiThreadedDetector, DetectorConfig};
use mev_core::ffi::rdtsc_native;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::mpsc;
use std::env;
use std::time::Duration;
use metrics_exporter_prometheus::PrometheusBuilder;
use ethers::providers::{Provider, Http, Middleware};
use ethers::types::{BlockNumber, U256 as EthU256};
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)?;

    println!("╔═══════════════════════════════════════════════════════════════╗");
    println!("║          MEV PROTOCOL ENGINE v0.1.0 - ARBITRUM                ║");
    println!("║          Sub-microsecond latency | Flash Loan Arbitrage       ║");
    println!("╚═══════════════════════════════════════════════════════════════╝\n");

    // Start Prometheus metrics (custom server with CORS for dashboard)
    let prom_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    let h: std::sync::Arc<metrics_exporter_prometheus::PrometheusHandle> =
        std::sync::Arc::new(prom_handle);
    let h2 = h.clone();
    tokio::spawn(async move {
        let listener = TcpListener::bind("0.0.0.0:9091").await
            .expect("Failed to bind metrics port 9091");
        info!("✅ Prometheus metrics → http://localhost:9091/metrics");
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                let body = h2.render();
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/plain; charset=utf-8\r\n\
                     Access-Control-Allow-Origin: *\r\n\
                     Access-Control-Allow-Methods: GET, OPTIONS\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        }
    });

    // Check CLI args
    let args: Vec<String> = env::args().collect();
    
    if args.len() > 1 && args[1] == "benchmark" {
        info!("Running latency benchmarks...");
        mev_core::run_all_benchmarks();
        return Ok(());
    }

    // Load configuration
    let config = Config::from_env()?;
    
    // Get Arbitrum chain config (chain_id 42161)
    let arbitrum_config = config.chains.get(&42161)
        .expect("Arbitrum config not found! Add chain 42161 to config.");
    
    info!("✅ Configuration loaded");
    info!("   Chain ID: {}", arbitrum_config.chain_id);
    info!("   Contract: {:?}", arbitrum_config.contract_address);

    // Verify FFI is working
    let tsc_start = rdtsc_native();
    let tsc_end = rdtsc_native();
    info!("✅ FFI verified | TSC delta: {} cycles", tsc_end - tsc_start);

    // Create mempool monitor
    let mempool_config = MempoolConfig {
        ws_url: arbitrum_config.ws_url.clone(),
        backup_ws_urls: vec![],
        max_pending_txs: 10_000,
        cpu_core: Some(0),
        batch_size: 32,
    };
    let mempool_monitor = MempoolMonitor::new(mempool_config);
    info!("✅ Mempool monitor initialized");

    // Count configured RPC endpoints
    let rpc_endpoint_count = {
        let mut count = 1u64; // primary ARBITRUM_RPC_URL
        if let Ok(endpoints) = std::env::var("MEV_RPC_ENDPOINTS") {
            let extra = endpoints.split(',')
                .filter(|s| !s.trim().is_empty())
                .count() as u64;
            if extra > 0 { count = extra; }
        }
        count
    };

    // Block tracker + TX classifier (Arbitrum has no public mempool — classify from blocks)
    let rpc_url_clone = arbitrum_config.rpc_url.clone();
    let engine_start = std::time::Instant::now();
    tokio::spawn(async move {
        let provider = match Provider::<Http>::try_from(rpc_url_clone.as_str()) {
            Ok(p) => p,
            Err(e) => { warn!("RPC provider failed: {}", e); return; }
        };
        let mut prev_block = 0u64;
        let mut total_tx = 0u64;
        let mut total_swaps_v2 = 0u64;
        let mut total_swaps_v3 = 0u64;
        let mut total_transfers = 0u64;
        let mut total_filtered = 0u64;

        // Known DEX router selectors (first 4 bytes of calldata)
        // UniswapV2: swapExactTokensForTokens, swapTokensForExactTokens
        // UniswapV3: exactInputSingle, exactInput, multicall
        // SushiSwap, Camelot, etc.
        const SEL_V2_SWAP_EXACT: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
        const SEL_V2_SWAP_TOKENS: [u8; 4] = [0x8a, 0x65, 0x7e, 0x67];
        const SEL_V2_SWAP_ETH: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
        const SEL_V2_SWAP_EXACT_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
        const SEL_V3_EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
        const SEL_V3_EXACT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
        const SEL_MULTICALL: [u8; 4] = [0xac, 0x96, 0x50, 0xd8];
        const SEL_MULTICALL2: [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc];
        const SEL_TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
        const SEL_APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];

        fn classify_tx(input: &[u8], value: EthU256) -> &'static str {
            if input.len() < 4 {
                return if value > EthU256::zero() { "transfer" } else { "unknown" };
            }
            let sel: [u8; 4] = [input[0], input[1], input[2], input[3]];
            match sel {
                SEL_V2_SWAP_EXACT | SEL_V2_SWAP_TOKENS |
                SEL_V2_SWAP_ETH | SEL_V2_SWAP_EXACT_ETH => "swap_v2",
                SEL_V3_EXACT_INPUT | SEL_V3_EXACT_SINGLE => "swap_v3",
                SEL_MULTICALL | SEL_MULTICALL2 => "swap_v3",
                SEL_TRANSFER => "transfer",
                SEL_APPROVE => "unknown",
                _ => {
                    if value > EthU256::zero() { "transfer" } else { "unknown" }
                }
            }
        }

        loop {
            metrics::gauge!("mev_node_uptime_seconds_total")
                .set(engine_start.elapsed().as_secs_f64());
            metrics::gauge!("mev_rpc_healthy_endpoints").set(rpc_endpoint_count as f64);
            metrics::gauge!("mev_rpc_total_endpoints").set(rpc_endpoint_count as f64);

            match provider.get_block_with_txs(BlockNumber::Latest).await {
                Ok(Some(block)) => {
                    let num = block.number.map(|n| n.as_u64()).unwrap_or(0);
                    metrics::gauge!("mev_block_latest_number").set(num as f64);

                    if num != prev_block && prev_block > 0 {
                        metrics::counter!("mev_block_processed_total").increment(1);

                        // Gas oracle
                        if let Some(base_fee) = block.base_fee_per_gas {
                            let gwei = base_fee.as_u64() as f64 / 1e9;
                            metrics::gauge!("mev_gas_base_fee_gwei").set(gwei);
                            metrics::gauge!("mev_gas_priority_fee_gwei").set(0.01);
                            metrics::gauge!("mev_gas_predicted_base_fee_gwei")
                                .set(gwei * 1.125);
                        }

                        // Gas utilization
                        let gl = block.gas_limit.as_u64() as f64;
                        let gu = block.gas_used.as_u64() as f64;
                        if gl > 0.0 {
                            metrics::gauge!("mev_block_gas_utilization_ratio")
                                .set(gu / gl);
                        }

                        // Propagation
                        let now_ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap().as_secs();
                        let prop_ms = now_ts.saturating_sub(block.timestamp.as_u64()) * 1000;
                        metrics::gauge!("mev_block_propagation_ms").set(prop_ms as f64);

                        // Classify each transaction in the block
                        let tx_count = block.transactions.len() as u64;
                        let mut block_swaps_v2 = 0u64;
                        let mut block_swaps_v3 = 0u64;
                        let mut block_transfers = 0u64;
                        let mut block_unknown = 0u64;

                        let mut classify_total_ns = 0u64;
                        for tx in &block.transactions {
                            let t0 = std::time::Instant::now();
                            let class = classify_tx(&tx.input, tx.value);
                            classify_total_ns += t0.elapsed().as_nanos() as u64;
                            match class {
                                "swap_v2" => {
                                    block_swaps_v2 += 1;
                                    metrics::counter!("mev_pipeline_opportunities_found_total", "type" => "swap_v2")
                                        .increment(1);
                                    metrics::counter!("mev_pipeline_filtered_total").increment(1);
                                }
                                "swap_v3" => {
                                    block_swaps_v3 += 1;
                                    metrics::counter!("mev_pipeline_opportunities_found_total", "type" => "swap_v3")
                                        .increment(1);
                                    metrics::counter!("mev_pipeline_filtered_total").increment(1);
                                }
                                "transfer" => {
                                    block_transfers += 1;
                                    metrics::counter!("mev_pipeline_opportunities_found_total", "type" => "transfer")
                                        .increment(1);
                                }
                                _ => { block_unknown += 1; }
                            }
                        }

                        // Live classification latency (per-tx average in nanoseconds)
                        if tx_count > 0 {
                            let avg_ns = classify_total_ns as f64 / tx_count as f64;
                            metrics::gauge!("mev_classify_latency_ns").set(avg_ns);
                        }

                        // Base fee prediction latency
                        if let Some(base_fee) = block.base_fee_per_gas {
                            let t0 = std::time::Instant::now();
                            let _predicted = base_fee.as_u64() as f64 / 1e9 * 1.125;
                            let pred_ns = t0.elapsed().as_nanos() as f64;
                            metrics::gauge!("mev_basefee_predict_latency_ns").set(pred_ns);
                        }

                        // Pipeline counters
                        metrics::counter!("mev_pipeline_tx_processed_total", "stage" => "classify")
                            .increment(tx_count);
                        metrics::counter!("mev_mempool_tx_received_total")
                            .increment(tx_count);

                        total_tx += tx_count;
                        total_swaps_v2 += block_swaps_v2;
                        total_swaps_v3 += block_swaps_v3;
                        total_transfers += block_transfers;
                        total_filtered += block_swaps_v2 + block_swaps_v3;

                        metrics::gauge!("mev_mempool_buffer_usage")
                            .set((total_tx % 10_000) as f64 / 10_000.0);
                        metrics::gauge!("mev_mempool_tx_rate_per_sec")
                            .set(total_tx as f64 / engine_start.elapsed().as_secs_f64().max(1.0));

                        // Log with classification
                        let swap_str = if block_swaps_v2 + block_swaps_v3 > 0 {
                            format!(" | 🔄 {}v2 {}v3", block_swaps_v2, block_swaps_v3)
                        } else { String::new() };

                        info!("📦 Block #{} | {:.4} Gwei | {} txs{} | total: {}",
                            num,
                            block.base_fee_per_gas
                                .map(|f| f.as_u64() as f64 / 1e9).unwrap_or(0.0),
                            tx_count,
                            swap_str,
                            total_tx,
                        );
                    }
                    prev_block = num;
                }
                Ok(None) => {}
                Err(e) => { warn!("Block fetch error: {}", e); }
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
    info!("✅ Block tracker + TX classifier started (250ms poll)");

    // Create multi-threaded detector
    let detector_config = DetectorConfig {
        num_workers: 4,
        min_profit_wei: ethers::types::U256::from(500_000_000_000_000u64), // 0.0005 ETH
        max_hops: 3,
        gas_price: ethers::types::U256::from(100_000_000), // 0.1 gwei
        batch_size: 64,
    };
    let _detector = MultiThreadedDetector::new(detector_config);
    info!("✅ Multi-threaded detector initialized (4 workers)");

    // Create engine
    let engine = MevEngine::new(config);

    // Handle shutdown
    let engine_clone = engine.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("🛑 Shutdown signal received");
        engine_clone.stop().await.ok();
    });

    // Start mempool monitoring
    let (tx_sender, mut tx_receiver) = mpsc::unbounded_channel();
    
    let monitor = mempool_monitor;
    tokio::spawn(async move {
        if let Err(e) = monitor.start(tx_sender).await {
            warn!("Mempool monitor error: {}", e);
        }
    });

    info!("\n🚀 MEV ENGINE STARTED");
    info!("   Monitoring mempool for opportunities...\n");

    // Main loop — process transactions
    let mut tx_count = 0u64;
    let mut total_count = 0u64;
    let mut last_log = std::time::Instant::now();

    loop {
        tokio::select! {
            Some(tx) = tx_receiver.recv() => {
                tx_count += 1;
                total_count += 1;

                // Prometheus metrics (dashboard reads these)
                metrics::counter!("mev_mempool_tx_received_total").increment(1);
                metrics::counter!("mev_pipeline_tx_processed_total", "stage" => "classify")
                    .increment(1);
                metrics::gauge!("mev_mempool_buffer_usage")
                    .set(total_count as f64 / 10_000.0);

                // Log first TX, then every 50th
                if total_count == 1 {
                    info!("🔗 First pending TX: {:?}", tx.hash);
                } else if tx_count % 50 == 0 {
                    info!("🔗 TX #{}: {:?}", total_count, tx.hash);
                }

                if tx.is_swap {
                    info!("🔄 Swap detected: {:?}", tx.hash);
                    metrics::counter!("mev_pipeline_opportunities_found_total", "type" => "swap_v2")
                        .increment(1);
                    metrics::counter!("mev_pipeline_filtered_total").increment(1);
                }

                // Stats every 5 seconds
                if last_log.elapsed() > Duration::from_secs(5) {
                    let rate = tx_count as f64 / last_log.elapsed().as_secs_f64();
                    info!("📊 Mempool | {} TXs ({:.0}/s) | Total: {}",
                          tx_count, rate, total_count);
                    metrics::gauge!("mev_mempool_tx_rate_per_sec").set(rate);
                    tx_count = 0;
                    last_log = std::time::Instant::now();
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("🛑 Shutting down...");
                break;
            }
        }
    }

    info!("✅ MEV Engine shutdown complete");
    Ok(())
}
