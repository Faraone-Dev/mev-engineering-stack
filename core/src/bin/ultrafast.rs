// ⚡ ULTRA-FAST MEV LAUNCHER v3
// Optimizations: Multi-RPC, targeted refresh, pre-computed paths

use ethers::{
    prelude::*,
    providers::{Provider, Http, Ws},
    types::{Address, U256, TransactionRequest},
    abi::{Token, encode},
};
use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::str::FromStr;

// ============ CONFIG ============
// Multiple RPC endpoints for parallel/fallback (all free, no API key needed)
const RPC_URLS: &[&str] = &[
    "https://arb-mainnet.g.alchemy.com/v2/ojTHnyjbleuh-3EVw2Z25",
    "https://arb1.arbitrum.io/rpc",           // Arbitrum official
    "https://arbitrum.blockpi.network/v1/rpc/public",  // BlockPI
    "https://1rpc.io/arb",                    // 1RPC
    "https://arbitrum.drpc.org",              // dRPC
];
const WS_URL: &str = "wss://arb-mainnet.g.alchemy.com/v2/ojTHnyjbleuh-3EVw2Z25";
const MIN_PROFIT_BPS: u32 = 10;

// Arbiscan API (Etherscan v2 for Arbitrum)
const ARBISCAN_API_KEY: &str = "YourArbiscanApiKey"; // Get free key at arbiscan.io
const ARBISCAN_API: &str = "https://api.arbiscan.io/api";

// Multicall3 (same on all chains)
const MULTICALL3: &str = "0xcA11bde05977b3631167028862bE2a173976CA11";

// ============ TOKENS ============
fn get_tokens() -> Vec<(&'static str, Address)> {
    vec![
        ("WETH", addr("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1")),
        ("USDC", addr("0xaf88d065e77c8cC2239327C5EDb3A432268e5831")),
        ("USDT", addr("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9")),
        ("ARB", addr("0x912CE59144191C1204E64559FE8253a0e49E6548")),
        ("WBTC", addr("0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f")),
        ("GMX", addr("0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a")),
        ("LINK", addr("0xf97f4df75117a78c1A5a0DBb814Af92458539FB4")),
        ("UNI", addr("0xFa7F8980b0f1E64A2062791cc3b0871572f1F7f0")),
        ("PENDLE", addr("0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8")),
        ("MAGIC", addr("0x539bdE0d7Dbd336b79148AA742883198BBF60342")),
        ("GRAIL", addr("0x3d9907F9a368ad0a51Be60f7Da3b97cf940982D8")),
        ("RDNT", addr("0x3082CC23568eA640225c2467653dB90e9250AaA0")),
    ]
}

fn addr(s: &str) -> Address {
    Address::from_str(s).unwrap()
}

// ============ FACTORIES ============
fn factories() -> (Address, Address, Address) {
    (
        addr("0x1F98431c8aD98523631AE4a59f267346ea31F984"), // Uniswap V3
        addr("0xc35DADB65012eC5796536bD9864eD8773aBc74C4"), // SushiSwap
        addr("0x6EcCab422D763aC031210895C81787E87B43A652"), // Camelot
    )
}

// ============ POOL DATA ============
#[derive(Clone, Debug)]
struct PoolData {
    address: Address,
    token0: Address,
    token1: Address,
    is_v3: bool,
    fee_bps: u32,
    reserve0: u128,
    reserve1: u128,
}

impl PoolData {
    fn get_amount_out(&self, amount_in: u128, zero_for_one: bool) -> u128 {
        let (r_in, r_out) = if zero_for_one {
            (self.reserve0, self.reserve1)
        } else {
            (self.reserve1, self.reserve0)
        };
        
        if r_in == 0 || r_out == 0 {
            return 0;
        }
        
        let fee_factor = 10000u128 - self.fee_bps as u128;
        let amount_in_with_fee = amount_in * fee_factor;
        let numerator = amount_in_with_fee * r_out;
        let denominator = r_in * 10000 + amount_in_with_fee;
        
        numerator / denominator
    }
}

// ============ OPPORTUNITY ============
#[derive(Clone, Debug)]
struct Opportunity {
    token_name: String,
    buy_pool: Address,
    sell_pool: Address,
    amount_in: u128,
    profit_bps: i32,
    profit_eth: f64,
}

// ============ RAW MULTICALL ============
/// Encode tryAggregate call with (target, calldata)[]
/// tryAggregate(bool requireSuccess, (address,bytes)[] calls) returns (bool,bytes)[]
fn encode_multicall(calls: &[(Address, Vec<u8>)]) -> Vec<u8> {
    // tryAggregate(bool,(address,bytes)[])
    // selector = 0xbce38bd7
    let mut data = vec![0xbc, 0xe3, 0x8b, 0xd7];
    
    // ABI encode: requireSuccess = false, then array of (address, bytes) tuples
    let encoded_calls: Vec<Token> = calls.iter().map(|(target, calldata)| {
        Token::Tuple(vec![
            Token::Address(*target),
            Token::Bytes(calldata.clone()),
        ])
    }).collect();
    
    let encoded = encode(&[
        Token::Bool(false), // requireSuccess = false
        Token::Array(encoded_calls),
    ]);
    data.extend(encoded);
    
    data
}

/// Parse tryAggregate response - returns Vec<Option<Vec<u8>>>
/// Response is (bool success, bytes data)[]
fn parse_multicall_response(data: &[u8]) -> Vec<Option<Vec<u8>>> {
    if data.len() < 64 {
        return vec![];
    }
    
    // Decode using ethers abi
    use ethers::abi::{decode, ParamType};
    
    let result_type = ParamType::Array(Box::new(
        ParamType::Tuple(vec![
            ParamType::Bool,
            ParamType::Bytes,
        ])
    ));
    
    match decode(&[result_type], data) {
        Ok(tokens) => {
            if let Some(Token::Array(results)) = tokens.into_iter().next() {
                results.into_iter().map(|token| {
                    if let Token::Tuple(mut tuple) = token {
                        if tuple.len() == 2 {
                            let success = if let Token::Bool(b) = tuple.remove(0) { b } else { false };
                            let bytes = if let Token::Bytes(b) = tuple.remove(0) { b } else { vec![] };
                            if success && !bytes.is_empty() {
                                return Some(bytes);
                            }
                        }
                    }
                    None
                }).collect()
            } else {
                vec![]
            }
        }
        Err(_) => vec![],
    }
}

// ============ ULTRA-FAST POOL MANAGER ============
struct UltraFastPoolManager {
    http_providers: Vec<Arc<Provider<Http>>>,
    ws_provider: Option<Arc<Provider<Ws>>>,
    multicall_addr: Address,
    v2_pools: Vec<(Address, Address, Address, u32)>,
    v3_pools: Vec<(Address, Address, Address, u32)>,
    pools: HashMap<Address, PoolData>,
    // Pre-computed arbitrage paths for faster checking
    arb_paths: Vec<ArbPath>,
}

#[derive(Clone, Debug)]
struct ArbPath {
    token_name: String,
    token: Address,
    pool_a_idx: usize,
    pool_b_idx: usize,
}

impl UltraFastPoolManager {
    fn new(http_providers: Vec<Arc<Provider<Http>>>) -> Self {
        Self {
            http_providers,
            ws_provider: None,
            multicall_addr: addr(MULTICALL3),
            v2_pools: Vec::new(),
            v3_pools: Vec::new(),
            pools: HashMap::new(),
            arb_paths: Vec::new(),
        }
    }
    
    fn set_ws_provider(&mut self, ws: Arc<Provider<Ws>>) {
        self.ws_provider = Some(ws);
    }
    
    /// Get fastest provider (first one)
    fn fast_provider(&self) -> &Arc<Provider<Http>> {
        &self.http_providers[0]
    }
    
    /// Discover all pools (one-time)
    async fn discover_pools(&mut self) {
        let (v3_factory, sushi_factory, camelot_factory) = factories();
        let tokens = get_tokens();
        
        println!("🔍 Discovering pools with Multicall3...");
        let start = Instant::now();
        
        // Build multicall for pool discovery
        let mut calls: Vec<(Address, Vec<u8>)> = Vec::new();
        let mut call_metadata: Vec<(Address, Address, bool, u32)> = Vec::new();
        
        for i in 0..tokens.len() {
            for j in (i+1)..tokens.len() {
                let (_, t0) = tokens[i];
                let (_, t1) = tokens[j];
                let (token0, token1) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
                
                // V3 pools (3 fee tiers)
                for fee in [500u32, 3000, 10000] {
                    let mut calldata = vec![0x16, 0x98, 0xee, 0x82]; // getPool selector
                    calldata.extend(&encode(&[
                        Token::Address(token0),
                        Token::Address(token1),
                        Token::Uint(U256::from(fee)),
                    ]));
                    calls.push((v3_factory, calldata));
                    call_metadata.push((token0, token1, true, fee));
                }
                
                // SushiSwap
                let mut sushi_call = vec![0xe6, 0xa4, 0x39, 0x05]; // getPair selector
                sushi_call.extend(&encode(&[
                    Token::Address(token0),
                    Token::Address(token1),
                ]));
                calls.push((sushi_factory, sushi_call));
                call_metadata.push((token0, token1, false, 30));
                
                // Camelot
                let mut camelot_call = vec![0xe6, 0xa4, 0x39, 0x05];
                camelot_call.extend(&encode(&[
                    Token::Address(token0),
                    Token::Address(token1),
                ]));
                calls.push((camelot_factory, camelot_call));
                call_metadata.push((token0, token1, false, 30));
            }
        }
        
        println!("   Making {} discovery calls in ONE RPC...", calls.len());
        
        // Execute multicall
        let calldata = encode_multicall(&calls);
        let tx = TransactionRequest::new()
            .to(self.multicall_addr)
            .data(calldata);
        
        match self.fast_provider().call(&tx.into(), None).await {
            Ok(result) => {
                let parsed = parse_multicall_response(&result);
                
                for (i, maybe_data) in parsed.iter().enumerate() {
                    if let Some(data) = maybe_data {
                        if data.len() >= 32 {
                            let pool_addr = Address::from_slice(&data[12..32]);
                            if pool_addr != Address::zero() {
                                if i < call_metadata.len() {
                                    let (token0, token1, is_v3, fee) = call_metadata[i];
                                    if is_v3 {
                                        self.v3_pools.push((pool_addr, token0, token1, fee / 100));
                                    } else {
                                        self.v2_pools.push((pool_addr, token0, token1, fee));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ Discovery failed: {}", e);
            }
        }
        
        println!("✅ Found {} V2 pools + {} V3 pools in {:?}", 
            self.v2_pools.len(), self.v3_pools.len(), start.elapsed());
    }
    
    /// ULTRA-FAST refresh - ONE RPC call for ALL pools with racing
    async fn refresh_all(&mut self) -> Duration {
        let start = Instant::now();
        
        let mut calls: Vec<(Address, Vec<u8>)> = Vec::new();
        
        // V2: getReserves() = 0x0902f1ac
        for (pool, _, _, _) in &self.v2_pools {
            calls.push((*pool, vec![0x09, 0x02, 0xf1, 0xac]));
        }
        
        // V3: slot0() = 0x3850c7bd, liquidity() = 0x1a686502
        for (pool, _, _, _) in &self.v3_pools {
            calls.push((*pool, vec![0x38, 0x50, 0xc7, 0xbd]));
            calls.push((*pool, vec![0x1a, 0x68, 0x65, 0x02]));
        }
        
        let calldata = encode_multicall(&calls);
        let tx = TransactionRequest::new()
            .to(self.multicall_addr)
            .data(calldata);
        
        // Race all providers - use first SUCCESSFUL response
        let result = if let Some(ws) = &self.ws_provider {
            // WebSocket is usually faster, just use it
            ws.call(&tx.clone().into(), None).await
        } else if self.http_providers.len() > 1 {
            // Race multiple HTTP providers - take first success
            use futures::future::select_all;
            use tokio::time::timeout;
            
            let futures: Vec<_> = self.http_providers.iter()
                .map(|p| {
                    let tx = tx.clone();
                    let p = p.clone();
                    async move { 
                        // 500ms timeout per provider
                        match timeout(Duration::from_millis(500), p.call(&tx.into(), None)).await {
                            Ok(result) => result,
                            Err(_) => Err(ethers::providers::ProviderError::CustomError("timeout".into())),
                        }
                    }
                })
                .map(Box::pin)
                .collect();
            
            // Keep racing until we get a success or all fail
            let mut remaining = futures;
            let mut final_result = Err(ethers::providers::ProviderError::CustomError("all failed".into()));
            
            while !remaining.is_empty() {
                let (result, _idx, rest) = select_all(remaining).await;
                if result.is_ok() {
                    final_result = result;
                    break;
                }
                remaining = rest;
            }
            
            final_result
        } else {
            self.fast_provider().call(&tx.into(), None).await
        };
        
        match result {
            Ok(result) => {
                let parsed = parse_multicall_response(&result);
                
                // Parse V2 results
                for (i, (pool_addr, t0, t1, fee)) in self.v2_pools.iter().enumerate() {
                    if let Some(data) = parsed.get(i).and_then(|d| d.clone()) {
                        if data.len() >= 64 {
                            let reserve0 = U256::from_big_endian(&data[0..32]).as_u128();
                            let reserve1 = U256::from_big_endian(&data[32..64]).as_u128();
                            
                            if reserve0 > 1000 && reserve1 > 1000 {
                                self.pools.insert(*pool_addr, PoolData {
                                    address: *pool_addr,
                                    token0: *t0,
                                    token1: *t1,
                                    is_v3: false,
                                    fee_bps: *fee,
                                    reserve0,
                                    reserve1,
                                });
                            }
                        }
                    }
                }
                
                // Parse V3 results
                let v3_start = self.v2_pools.len();
                for (i, (pool_addr, t0, t1, fee)) in self.v3_pools.iter().enumerate() {
                    let slot0_data = parsed.get(v3_start + i * 2).and_then(|d| d.clone());
                    let liq_data = parsed.get(v3_start + i * 2 + 1).and_then(|d| d.clone());
                    
                    if let (Some(s0), Some(liq)) = (slot0_data, liq_data) {
                        if s0.len() >= 32 && liq.len() >= 32 {
                            let sqrt_price = U256::from_big_endian(&s0[0..32]);
                            let liquidity = U256::from_big_endian(&liq[0..32]).as_u128();
                            
                            if liquidity > 0 && !sqrt_price.is_zero() {
                                let q96 = U256::from(1u128) << 96;
                                let liq_u256 = U256::from(liquidity);
                                let reserve0 = ((liq_u256 * q96) / sqrt_price).as_u128();
                                let reserve1 = ((liq_u256 * sqrt_price) / q96).as_u128();
                                
                                if reserve0 > 1000 && reserve1 > 1000 {
                                    self.pools.insert(*pool_addr, PoolData {
                                        address: *pool_addr,
                                        token0: *t0,
                                        token1: *t1,
                                        is_v3: true,
                                        fee_bps: *fee,
                                        reserve0,
                                        reserve1,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("❌ Refresh failed: {}", e);
            }
        }
        
        start.elapsed()
    }
    
    /// Find arbitrage opportunities
    fn find_opportunities(&self, weth: Address) -> Vec<Opportunity> {
        let tokens = get_tokens();
        let mut opportunities = Vec::new();
        let mut best_ratio = 0.0f64;
        let mut best_pair = String::new();
        
        let test_amounts = [
            100_000_000_000_000_000u128,   // 0.1 ETH
            500_000_000_000_000_000u128,   // 0.5 ETH
            1_000_000_000_000_000_000u128, // 1 ETH
            5_000_000_000_000_000_000u128, // 5 ETH
        ];
        
        for (token_name, token) in &tokens {
            if *token == weth {
                continue;
            }
            
            let weth_pools: Vec<&PoolData> = self.pools.values()
                .filter(|p| {
                    (p.token0 == weth && p.token1 == *token) ||
                    (p.token0 == *token && p.token1 == weth)
                })
                .collect();
            
            if weth_pools.len() < 2 {
                continue;
            }
            
            for buy_pool in &weth_pools {
                for sell_pool in &weth_pools {
                    if buy_pool.address == sell_pool.address {
                        continue;
                    }
                    
                    for &amount_in in &test_amounts {
                        let buy_zero_for_one = buy_pool.token0 == weth;
                        let tokens_bought = buy_pool.get_amount_out(amount_in, buy_zero_for_one);
                        
                        if tokens_bought == 0 {
                            continue;
                        }
                        
                        let sell_zero_for_one = sell_pool.token0 != weth;
                        let weth_out = sell_pool.get_amount_out(tokens_bought, sell_zero_for_one);
                        
                        if weth_out == 0 {
                            continue;
                        }
                        
                        let ratio = weth_out as f64 / amount_in as f64;
                        
                        if ratio > best_ratio {
                            best_ratio = ratio;
                            best_pair = token_name.to_string();
                        }
                        
                        if weth_out > amount_in {
                            let profit_bps = ((ratio - 1.0) * 10000.0) as i32;
                            
                            if profit_bps >= MIN_PROFIT_BPS as i32 {
                                let profit_eth = (weth_out - amount_in) as f64 / 1e18;
                                
                                opportunities.push(Opportunity {
                                    token_name: token_name.to_string(),
                                    buy_pool: buy_pool.address,
                                    sell_pool: sell_pool.address,
                                    amount_in,
                                    profit_bps,
                                    profit_eth,
                                });
                            }
                        }
                    }
                }
            }
        }
        
        if opportunities.is_empty() && best_ratio > 0.0 {
            let bps = ((best_ratio - 1.0) * 10000.0) as i32;
            print!("\r📊 Best: {:.6} ({}bps) on WETH/{} | Pools: {}    ", 
                best_ratio, bps, best_pair, self.pools.len());
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        
        opportunities.sort_by(|a, b| b.profit_bps.cmp(&a.profit_bps));
        opportunities
    }
}

// ============ MAIN ============
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("
╔═══════════════════════════════════════════════════════════╗
║     ⚡ ULTRA-FAST MEV LAUNCHER v2 ⚡                       ║
║     Target: <5ms refresh via raw Multicall3               ║
╚═══════════════════════════════════════════════════════════╝
");

    // Initialize multiple RPC providers for redundancy/speed testing
    let mut providers = Vec::new();
    for url in RPC_URLS {
        if let Ok(p) = Provider::<Http>::try_from(*url) {
            providers.push(Arc::new(p));
            println!("✅ Connected to: {}", url.split('/').take(3).collect::<Vec<_>>().join("/"));
        }
    }
    
    if providers.is_empty() {
        println!("❌ No RPC providers available!");
        return Ok(());
    }
    
    let weth = addr("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1");
    
    let mut manager = UltraFastPoolManager::new(providers);
    
    manager.discover_pools().await;
    
    let duration = manager.refresh_all().await;
    println!("🚀 Initial refresh (HTTP): {:?} ({} active pools)", duration, manager.pools.len());
    
    println!("\n🔗 Connecting to WebSocket for block updates AND low-latency calls...");
    let ws = Arc::new(Provider::<Ws>::connect(WS_URL).await?);
    manager.set_ws_provider(ws.clone());
    
    // Test WS latency
    let ws_test = manager.refresh_all().await;
    println!("🚀 WS refresh test: {:?}", ws_test);
    
    let mut block_stream = ws.subscribe_blocks().await?;
    
    println!("\n⚡ Running ultra-fast arbitrage loop...\n");
    
    let mut total_refreshes = 0u64;
    let mut total_time = Duration::ZERO;
    let mut min_time = Duration::from_secs(999);
    let mut max_time = Duration::ZERO;
    
    loop {
        tokio::select! {
            Some(block) = block_stream.next() => {
                let refresh_time = manager.refresh_all().await;
                
                total_refreshes += 1;
                total_time += refresh_time;
                if refresh_time < min_time { min_time = refresh_time; }
                if refresh_time > max_time { max_time = refresh_time; }
                
                let opps = manager.find_opportunities(weth);
                
                if !opps.is_empty() {
                    println!("\n\n🎯 OPPORTUNITY FOUND!");
                    for opp in opps.iter().take(3) {
                        println!("   {} | +{} bps | +{:.6} ETH | Amount: {:.4} ETH",
                            opp.token_name,
                            opp.profit_bps,
                            opp.profit_eth,
                            opp.amount_in as f64 / 1e18);
                    }
                    println!();
                }
                
                if total_refreshes % 10 == 0 {
                    let avg = total_time / total_refreshes as u32;
                    println!("\n📈 Block #{} | Refreshes: {} | Avg: {:?} | Min: {:?} | Max: {:?}",
                        block.number.unwrap_or_default(),
                        total_refreshes,
                        avg,
                        min_time,
                        max_time);
                }
            }
        }
    }
}
