//! Ultra-low latency WebSocket mempool monitor
//! Zero-copy parsing, CPU pinning, lock-free queues

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, error, debug};
use ethers::types::{Transaction, H256, U256, Address};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Helper function for getting timestamp
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

/// Mempool transaction with timing info
#[derive(Debug, Clone)]
pub struct MempoolTx {
    pub hash: H256,
    pub tx: Transaction,
    pub first_seen_tsc: u64,      // CPU timestamp counter
    pub first_seen_ns: u64,       // Nanoseconds
    pub gas_price: U256,
    pub is_swap: bool,
    pub swap_info: Option<SwapInfo>,
}

/// Ultra-fast mempool monitor config
#[derive(Clone)]
pub struct MempoolConfig {
    pub ws_url: String,
    pub backup_ws_urls: Vec<String>,
    pub max_pending_txs: usize,
    pub cpu_core: Option<usize>,       // Pin to specific CPU core
    pub batch_size: usize,              // Process in batches
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            ws_url: String::new(),
            backup_ws_urls: Vec::new(),
            max_pending_txs: 10_000,
            cpu_core: Some(0),           // Pin to core 0
            batch_size: 32,
        }
    }
}

/// Statistics for performance monitoring
#[derive(Default)]
pub struct MempoolStats {
    pub txs_received: AtomicU64,
    pub txs_parsed: AtomicU64,
    pub swaps_detected: AtomicU64,
    pub avg_latency_ns: AtomicU64,
    pub min_latency_ns: AtomicU64,
    pub max_latency_ns: AtomicU64,
}

/// Ultra-low latency mempool monitor
pub struct MempoolMonitor {
    config: MempoolConfig,
    running: Arc<AtomicBool>,
    stats: Arc<MempoolStats>,
}

impl MempoolMonitor {
    pub fn new(config: MempoolConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(MempoolStats::default()),
        }
    }
    
    /// Start monitoring with CPU pinning
    pub async fn start(
        &self,
        tx_sender: mpsc::UnboundedSender<MempoolTx>,
    ) -> anyhow::Result<()> {
        self.running.store(true, Ordering::SeqCst);
        
        // Pin to CPU core if specified
        if let Some(_core) = self.config.cpu_core {
            #[cfg(target_os = "linux")]
            {
                use core_affinity::CoreId;
                let core_ids = core_affinity::get_core_ids().unwrap_or_default();
                if let Some(core_id) = core_ids.get(core) {
                    core_affinity::set_for_current(*core_id);
                    info!("Pinned mempool monitor to CPU core {}", core);
                }
            }
        }
        
        info!("Connecting to WebSocket: {}", self.config.ws_url);
        
        // Connect with low-latency TCP options
        let (ws_stream, _) = connect_async(&self.config.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();
        
        // Subscribe to pending transactions
        let subscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["newPendingTransactions"]
        });
        
        write.send(Message::Text(subscribe_msg.to_string())).await?;
        info!("Subscribed to pending transactions");
        
        // Pre-allocate buffers
        let mut pending_hashes: Vec<H256> = Vec::with_capacity(self.config.batch_size);
        
        while self.running.load(Ordering::SeqCst) {
            tokio::select! {
                Some(msg) = read.next() => {
                    let receive_tsc = rdtsc();
                    let receive_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as u64;
                    
                    match msg {
                        Ok(Message::Text(text)) => {
                            self.stats.txs_received.fetch_add(1, Ordering::Relaxed);
                            
                            // Zero-copy JSON parsing for tx hash
                            if let Some(hash) = self.extract_tx_hash_fast(&text) {
                                pending_hashes.push(hash);
                                
                                // Batch processing
                                if pending_hashes.len() >= self.config.batch_size {
                                    self.process_batch(
                                        &pending_hashes,
                                        &tx_sender,
                                        receive_tsc,
                                        receive_ns,
                                    ).await;
                                    pending_hashes.clear();
                                }
                            }
                        }
                        Ok(Message::Binary(data)) => {
                            // Handle binary format if provider supports it
                            debug!("Received binary message: {} bytes", data.len());
                        }
                        Ok(Message::Ping(data)) => {
                            if let Err(e) = write.send(Message::Pong(data)).await {
                                warn!(error = %e, "Failed to send WebSocket pong");
                            }
                        }
                        Err(e) => {
                            error!("WebSocket error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_micros(100)) => {
                    // Process remaining batch
                    if !pending_hashes.is_empty() {
                        let tsc = rdtsc();
                        let ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos() as u64;
                        self.process_batch(&pending_hashes, &tx_sender, tsc, ns).await;
                        pending_hashes.clear();
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Ultra-fast tx hash extraction without full JSON parsing
    #[inline(always)]
    fn extract_tx_hash_fast(&self, text: &str) -> Option<H256> {
        // Look for "result":"0x... pattern
        const RESULT_PATTERN: &str = "\"result\":\"0x";
        
        if let Some(start) = text.find(RESULT_PATTERN) {
            let hash_start = start + RESULT_PATTERN.len();
            if text.len() >= hash_start + 64 {
                let hash_str = &text[hash_start..hash_start + 64];
                if let Ok(bytes) = hex::decode(hash_str) {
                    if bytes.len() == 32 {
                        return Some(H256::from_slice(&bytes));
                    }
                }
            }
        }
        None
    }
    
    /// Process batch of tx hashes
    async fn process_batch(
        &self,
        hashes: &[H256],
        tx_sender: &mpsc::UnboundedSender<MempoolTx>,
        receive_tsc: u64,
        receive_ns: u64,
    ) {
        for hash in hashes {
            // Dispatch hash with timing metadata. The detector pipeline fetches
            // full tx data via eth_getTransactionByHash in its own batch loop,
            // keeping this ingest path allocation-free and latency-bounded.
            let mempool_tx = MempoolTx {
                hash: *hash,
                tx: Transaction::default(),
                first_seen_tsc: receive_tsc,
                first_seen_ns: receive_ns,
                gas_price: U256::zero(),
                is_swap: false,
                swap_info: None,
            };
            
            if tx_sender.send(mempool_tx).is_err() {
                warn!("Tx channel full, dropping transaction");
            }
        }
        
        self.stats.txs_parsed.fetch_add(hashes.len() as u64, Ordering::Relaxed);
    }
    
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
    
    pub fn stats(&self) -> &MempoolStats {
        &self.stats
    }
}

/// Direct mempool subscription with full tx data (Alchemy enhanced API)
pub struct EnhancedMempoolMonitor {
    config: MempoolConfig,
    running: Arc<AtomicBool>,
}

impl EnhancedMempoolMonitor {
    pub fn new(config: MempoolConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
        }
    }
    
    /// Subscribe to Alchemy's enhanced API with full pending txs
    pub async fn start_enhanced(
        &self,
        tx_sender: mpsc::UnboundedSender<MempoolTx>,
    ) -> anyhow::Result<()> {
        self.running.store(true, Ordering::SeqCst);
        
        let (ws_stream, _) = connect_async(&self.config.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();
        
        // Alchemy enhanced subscription - gets full tx data
        let subscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": [
                "alchemy_pendingTransactions",
                {
                    "toAddress": [],  // All addresses
                    "hashesOnly": false  // Get full tx
                }
            ]
        });
        
        write.send(Message::Text(subscribe_msg.to_string())).await?;
        info!("Subscribed to Alchemy enhanced pending transactions");
        
        while self.running.load(Ordering::SeqCst) {
            if let Some(Ok(Message::Text(text))) = read.next().await {
                let receive_tsc = crate::ffi::rdtsc_native();
                let receive_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                
                // Parse full transaction
                if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(result) = response.get("params").and_then(|p| p.get("result")) {
                        if let Ok(tx) = serde_json::from_value::<Transaction>(result.clone()) {
                            let is_swap = self.is_likely_swap(&tx);
                            let swap_info = if is_swap {
                                self.parse_swap_fast(&tx)
                            } else {
                                None
                            };
                            
                            let mempool_tx = MempoolTx {
                                hash: tx.hash,
                                gas_price: tx.gas_price.unwrap_or_default(),
                                is_swap,
                                swap_info,
                                tx,
                                first_seen_tsc: receive_tsc,
                                first_seen_ns: receive_ns,
                            };
                            
                            if let Err(e) = tx_sender.send(mempool_tx) {
                                warn!(error = %e, "Mempool channel full, dropping tx");
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Fast swap detection based on function selector
    #[inline(always)]
    fn is_likely_swap(&self, tx: &Transaction) -> bool {
        if tx.input.len() < 4 {
            return false;
        }
        
        let selector = &tx.input[0..4];
        
        // Common swap selectors
        matches!(selector, 
            // UniswapV2
            [0x38, 0xed, 0x17, 0x39] |  // swapExactTokensForTokens
            [0x7f, 0xf3, 0x6a, 0xb5] |  // swapExactETHForTokens
            [0x18, 0xcb, 0xaf, 0xe5] |  // swapExactTokensForETH
            // UniswapV3
            [0xc0, 0x4b, 0x8d, 0x59] |  // exactInputSingle
            [0xb8, 0x58, 0x18, 0x3f] |  // exactInput
            [0x41, 0x4b, 0xf3, 0x89] |  // exactOutputSingle
            // Universal Router
            [0x36, 0x93, 0xd8, 0xa4] |  // execute
            [0x24, 0x85, 0x6b, 0xc3]    // execute with deadline
        )
    }
    
    /// Fast swap parsing — decodes token addresses and amounts from ABI-encoded calldata.
    /// Supports V2 (swapExactTokensForTokens, swapExactETHForTokens, swapExactTokensForETH)
    /// and V3 (exactInputSingle). Returns None for unsupported selectors.
    fn parse_swap_fast(&self, tx: &Transaction) -> Option<SwapInfo> {
        let data = &tx.input;
        if data.len() < 68 {
            return None;
        }

        let selector: [u8; 4] = data[0..4].try_into().ok()?;

        match selector {
            // ── V2: swapExactTokensForTokens(uint256,uint256,address[],address,uint256) ──
            [0x38, 0xed, 0x17, 0x39] => {
                if data.len() < 4 + 5 * 32 {
                    return None;
                }
                let amount_in   = U256::from_big_endian(&data[4..36]);
                let min_out     = U256::from_big_endian(&data[36..68]);
                // path offset at word 2, path length at that offset, first two addresses
                let path_offset = U256::from_big_endian(&data[68..100]).as_usize() + 4;
                if data.len() < path_offset + 32 {
                    return None;
                }
                let path_len = U256::from_big_endian(&data[path_offset..path_offset + 32]).as_usize();
                if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 {
                    return None;
                }
                let token_in  = Address::from_slice(&data[path_offset + 32 + 12..path_offset + 32 + 32]);
                let token_out = Address::from_slice(&data[path_offset + 64 + 12..path_offset + 64 + 32]);
                Some(SwapInfo {
                    token_in, token_out, amount_in, min_amount_out: min_out,
                    dex_type: DexType::UniswapV2, pool_address: Address::zero(),
                })
            }
            // ── V2: swapExactETHForTokens(uint256,address[],address,uint256) ──
            [0x7f, 0xf3, 0x6a, 0xb5] => {
                if data.len() < 4 + 4 * 32 {
                    return None;
                }
                let min_out     = U256::from_big_endian(&data[4..36]);
                let path_offset = U256::from_big_endian(&data[36..68]).as_usize() + 4;
                if data.len() < path_offset + 32 {
                    return None;
                }
                let path_len = U256::from_big_endian(&data[path_offset..path_offset + 32]).as_usize();
                if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 {
                    return None;
                }
                let token_in  = Address::from_slice(&data[path_offset + 32 + 12..path_offset + 32 + 32]);
                let token_out = Address::from_slice(&data[path_offset + 64 + 12..path_offset + 64 + 32]);
                Some(SwapInfo {
                    token_in, token_out, amount_in: tx.value, min_amount_out: min_out,
                    dex_type: DexType::UniswapV2, pool_address: Address::zero(),
                })
            }
            // ── V2: swapExactTokensForETH(uint256,uint256,address[],address,uint256) ──
            [0x18, 0xcb, 0xaf, 0xe5] => {
                if data.len() < 4 + 5 * 32 {
                    return None;
                }
                let amount_in   = U256::from_big_endian(&data[4..36]);
                let min_out     = U256::from_big_endian(&data[36..68]);
                let path_offset = U256::from_big_endian(&data[68..100]).as_usize() + 4;
                if data.len() < path_offset + 32 {
                    return None;
                }
                let path_len = U256::from_big_endian(&data[path_offset..path_offset + 32]).as_usize();
                if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 {
                    return None;
                }
                let token_in  = Address::from_slice(&data[path_offset + 32 + 12..path_offset + 32 + 32]);
                let token_out = Address::from_slice(&data[path_offset + 64 + 12..path_offset + 64 + 32]);
                Some(SwapInfo {
                    token_in, token_out, amount_in, min_amount_out: min_out,
                    dex_type: DexType::UniswapV2, pool_address: Address::zero(),
                })
            }
            // ── V3: exactInputSingle(ExactInputSingleParams) ──
            // Params struct: tokenIn, tokenOut, fee, recipient, deadline, amountIn, amountOutMin, sqrtPriceLimitX96
            [0x41, 0x4b, 0xf3, 0x89] => {
                if data.len() < 4 + 8 * 32 {
                    return None;
                }
                let token_in  = Address::from_slice(&data[4 + 12..4 + 32]);
                let token_out = Address::from_slice(&data[36 + 12..36 + 32]);
                let amount_in = U256::from_big_endian(&data[4 + 5 * 32..4 + 6 * 32]);
                let min_out   = U256::from_big_endian(&data[4 + 6 * 32..4 + 7 * 32]);
                Some(SwapInfo {
                    token_in, token_out, amount_in, min_amount_out: min_out,
                    dex_type: DexType::UniswapV3, pool_address: Address::zero(),
                })
            }
            _ => None,
        }
    }
    
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// Swap info for mempool parsing
#[derive(Debug, Clone)]
pub struct SwapInfo {
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub min_amount_out: U256,
    pub dex_type: DexType,
    pub pool_address: Address,
}

#[derive(Debug, Clone, Copy)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    Camelot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tx_hash_correct_value() {
        let monitor = MempoolMonitor::new(MempoolConfig::default());
        let msg = r#"{"jsonrpc":"2.0","method":"eth_subscription","params":{"subscription":"0x1","result":"0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"}}"#;

        let hash = monitor.extract_tx_hash_fast(msg);
        assert!(hash.is_some());
        let expected = H256::from_slice(
            &hex::decode("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef").unwrap(),
        );
        assert_eq!(hash.unwrap(), expected);
    }

    #[test]
    fn test_extract_tx_hash_missing_result() {
        let monitor = MempoolMonitor::new(MempoolConfig::default());
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscription"}"#;
        assert!(monitor.extract_tx_hash_fast(msg).is_none());
    }

    #[test]
    fn test_extract_tx_hash_truncated() {
        let monitor = MempoolMonitor::new(MempoolConfig::default());
        // Only 32 hex chars (16 bytes) instead of 64 (32 bytes)
        let msg = r#"{"result":"0x1234567890abcdef1234567890abcdef"}"#;
        assert!(monitor.extract_tx_hash_fast(msg).is_none());
    }

    #[test]
    fn test_extract_tx_hash_invalid_hex() {
        let monitor = MempoolMonitor::new(MempoolConfig::default());
        let msg = r#"{"result":"0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"}"#;
        assert!(monitor.extract_tx_hash_fast(msg).is_none());
    }

    // ── is_likely_swap tests (on EnhancedMempoolMonitor) ──

    fn make_tx_with_input(input: Vec<u8>) -> Transaction {
        let mut tx = Transaction::default();
        tx.input = input.into();
        tx
    }

    #[test]
    fn test_is_swap_v2_swap_exact_tokens() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        let tx = make_tx_with_input(vec![0x38, 0xed, 0x17, 0x39, 0x00, 0x00]);
        assert!(monitor.is_likely_swap(&tx));
    }

    #[test]
    fn test_is_swap_v3_exact_input_single() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        let tx = make_tx_with_input(vec![0x41, 0x4b, 0xf3, 0x89, 0x00]);
        assert!(monitor.is_likely_swap(&tx));
    }

    #[test]
    fn test_is_swap_universal_router_execute() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        let tx = make_tx_with_input(vec![0x36, 0x93, 0xd8, 0xa4, 0x00]);
        assert!(monitor.is_likely_swap(&tx));
    }

    #[test]
    fn test_is_not_swap_approve() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        // approve(address,uint256) = 0x095ea7b3
        let tx = make_tx_with_input(vec![0x09, 0x5e, 0xa7, 0xb3, 0x00]);
        assert!(!monitor.is_likely_swap(&tx), "approve() should NOT be classified as swap");
    }

    #[test]
    fn test_is_not_swap_transfer() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        // transfer(address,uint256) = 0xa9059cbb
        let tx = make_tx_with_input(vec![0xa9, 0x05, 0x9c, 0xbb, 0x00]);
        assert!(!monitor.is_likely_swap(&tx), "transfer() should NOT be classified as swap");
    }

    #[test]
    fn test_is_not_swap_empty_input() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        let tx = make_tx_with_input(vec![]);
        assert!(!monitor.is_likely_swap(&tx));
    }

    #[test]
    fn test_is_not_swap_short_input() {
        let monitor = EnhancedMempoolMonitor::new(MempoolConfig::default());
        let tx = make_tx_with_input(vec![0x38, 0xed]); // only 2 bytes
        assert!(!monitor.is_likely_swap(&tx));
    }
}
