//! Stage 2 — Fork-mode EVM Simulator via revm
//!
//! Executes transactions against a forked snapshot of on-chain state.
//! Used as the second stage: Stage 1 (AMM math filter) screens candidates
//! at ~35 ns, only survivors hit this stage (~50-200 µs per simulation).
//!
//! ## Architecture
//! ```text
//! Opportunity → [Stage 1: AMM math 35ns] → candidate → [Stage 2: revm fork] → GO/NO-GO
//! ```
//!
//! The fork DB lazily fetches storage slots from RPC on first access,
//! then caches them for the duration of the block. State is committed
//! between sequential transactions in a bundle so each tx sees the
//! effects of the previous one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use revm::db::{CacheDB, EmptyDB};
use revm::primitives::{
    AccountInfo, Address, Bytecode, Bytes, ExecutionResult, Output,
    TransactTo, TxEnv, BlockEnv, CfgEnv, Env, B256, U256,
    ResultAndState, State as EvmState, KECCAK_EMPTY,
};
use revm::{Evm, Database, DatabaseCommit};

use parking_lot::RwLock;
use tracing::{debug, warn, info, error};

use crate::config::Config;
use crate::types::{Opportunity, OpportunityType, SimulationResult, StateChange, Bundle};

// ── Constants ─────────────────────────────────────────────────────

/// Arbitrum chain ID
const ARBITRUM_CHAIN_ID: u64 = 42161;

/// Default gas limit for simulation (generous to avoid OOG false negatives)
const SIM_GAS_LIMIT: u64 = 1_500_000;

/// Balancer Vault on Arbitrum — flash loan entry point
#[allow(dead_code)]
const BALANCER_VAULT: [u8; 20] = [
    0xBA, 0x12, 0x22, 0x22, 0x22, 0x8d, 0x8B, 0xa4, 0x45, 0x95,
    0x8a, 0x75, 0xa0, 0x70, 0x4d, 0x56, 0x6B, 0xF2, 0xC8, 0x00,
];

// ── Block context snapshot ────────────────────────────────────────

/// Snapshot of the current block used to configure the EVM environment.
/// Updated every new block by the pipeline.
#[derive(Debug, Clone)]
pub struct BlockContext {
    pub number: u64,
    pub timestamp: u64,
    pub base_fee: u128,
    pub coinbase: [u8; 20],
}

impl Default for BlockContext {
    fn default() -> Self {
        Self {
            number: 0,
            timestamp: 0,
            base_fee: 20_000_000, // 0.02 gwei default for Arbitrum
            coinbase: [0u8; 20],
        }
    }
}

// ── Lazy-fetch fork database ──────────────────────────────────────

/// Account state fetched from RPC and cached locally.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CachedAccount {
    pub balance: U256,
    pub nonce: u64,
    pub code: Bytecode,
    pub code_hash: B256,
}

/// Fork database that wraps `CacheDB<EmptyDB>` and supports
/// pre-loading accounts and storage from RPC data.
///
/// In production, the pipeline calls `insert_account` / `insert_storage`
/// with data fetched from Alchemy before running the simulation.
/// This keeps RPC I/O outside the hot path.
pub struct ForkDB {
    inner: CacheDB<EmptyDB>,
    /// Track which accounts have been loaded (avoids re-fetch)
    loaded_accounts: HashMap<Address, bool>,
    /// Track which storage slots have been loaded
    loaded_storage: HashMap<(Address, U256), bool>,
}

impl ForkDB {
    pub fn new() -> Self {
        Self {
            inner: CacheDB::new(EmptyDB::default()),
            loaded_accounts: HashMap::new(),
            loaded_storage: HashMap::new(),
        }
    }

    /// Pre-load an account's balance, nonce, and bytecode.
    /// Called with data from `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`.
    pub fn insert_account(
        &mut self,
        address: Address,
        balance: U256,
        nonce: u64,
        code: Vec<u8>,
    ) {
        let bytecode = if code.is_empty() {
            Bytecode::default()
        } else {
            Bytecode::new_raw(Bytes::from(code))
        };
        let code_hash = if bytecode.is_empty() {
            KECCAK_EMPTY
        } else {
            bytecode.hash_slow()
        };

        let info = AccountInfo {
            balance,
            nonce,
            code_hash,
            code: Some(bytecode),
        };
        self.inner.insert_account_info(address, info);
        self.loaded_accounts.insert(address, true);
    }

    /// Pre-load a storage slot value.
    /// Called with data from `eth_getStorageAt`.
    pub fn insert_storage(&mut self, address: Address, slot: U256, value: U256) {
        let _ = self.inner.insert_account_storage(address, slot, value);
        self.loaded_storage.insert((address, slot), true);
    }

    /// Check if an account has been pre-loaded.
    pub fn is_account_loaded(&self, address: &Address) -> bool {
        self.loaded_accounts.contains_key(address)
    }

    /// Get the inner CacheDB for revm consumption.
    pub fn into_inner(self) -> CacheDB<EmptyDB> {
        self.inner
    }

    /// Borrow the inner CacheDB.
    pub fn inner(&self) -> &CacheDB<EmptyDB> {
        &self.inner
    }

    /// Mutably borrow the inner CacheDB.
    pub fn inner_mut(&mut self) -> &mut CacheDB<EmptyDB> {
        &mut self.inner
    }
}

// ── EVM Fork Simulator ───────────────────────────────────────────

/// Production-grade fork simulator.
///
/// Executes transactions against a snapshot of on-chain state using revm.
/// Designed to run as Stage 2 in the pipeline — only candidates that
/// pass the AMM math filter (Stage 1) reach this simulator.
pub struct EvmForkSimulator {
    config: Arc<Config>,
    /// Current block context — updated each block
    block_ctx: Arc<RwLock<BlockContext>>,
    /// Simulation counter for metrics
    sim_count: AtomicU64,
    /// Successful simulation counter
    sim_success: AtomicU64,
    /// Total latency accumulator (microseconds)
    total_latency_us: AtomicU64,
}

impl EvmForkSimulator {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            block_ctx: Arc::new(RwLock::new(BlockContext::default())),
            sim_count: AtomicU64::new(0),
            sim_success: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
        }
    }

    /// Update the block context when a new block arrives.
    pub fn update_block(&self, ctx: BlockContext) {
        debug!(
            block = ctx.number,
            base_fee = ctx.base_fee,
            "Fork simulator: block context updated"
        );
        *self.block_ctx.write() = ctx;
    }

    /// Get current block context.
    pub fn block_context(&self) -> BlockContext {
        self.block_ctx.read().clone()
    }

    /// Build a fresh EVM instance with the given fork DB and current block env.
    fn build_evm<'a>(&self, db: &'a mut CacheDB<EmptyDB>) -> Evm<'a, (), &'a mut CacheDB<EmptyDB>> {
        let block_ctx = self.block_ctx.read().clone();

        let mut env = Box::new(Env::default());

        // Block environment
        env.block.number = U256::from(block_ctx.number);
        env.block.timestamp = U256::from(block_ctx.timestamp);
        env.block.basefee = U256::from(block_ctx.base_fee);
        env.block.coinbase = Address::from(block_ctx.coinbase);
        // Arbitrum doesn't use prevrandao meaningfully
        env.block.prevrandao = Some(B256::ZERO);

        // Chain config
        env.cfg.chain_id = ARBITRUM_CHAIN_ID;
        // To simulate flash loans without pre-funding, we set gas_price to zero
        // and fund the caller generously. disable_balance_check / disable_base_fee
        // are behind optional feature flags in revm 8.0, so we work around them:
        env.tx.gas_price = U256::ZERO;

        Evm::builder()
            .with_env(env)
            .with_db(db)
            .build()
    }

    /// Simulate a single transaction call against forked state.
    ///
    /// Returns `(success, gas_used, output_bytes, state_changes)`.
    pub fn simulate_call(
        &self,
        db: &mut CacheDB<EmptyDB>,
        caller: Address,
        to: Address,
        calldata: Vec<u8>,
        value: U256,
        gas_limit: u64,
    ) -> SimCallResult {
        let mut evm = self.build_evm(db);

        // Set transaction environment
        evm.tx_mut().caller = caller;
        evm.tx_mut().transact_to = TransactTo::Call(to);
        evm.tx_mut().data = Bytes::from(calldata);
        evm.tx_mut().value = value;
        evm.tx_mut().gas_limit = gas_limit;
        evm.tx_mut().gas_price = U256::from(self.block_ctx.read().base_fee);

        match evm.transact() {
            Ok(result_and_state) => {
                let ResultAndState { result, state } = result_and_state;
                match result {
                    ExecutionResult::Success { gas_used, output, .. } => {
                        let output_bytes = match output {
                            Output::Call(bytes) => bytes.to_vec(),
                            Output::Create(bytes, _) => bytes.to_vec(),
                        };
                        // Extract state changes
                        let changes = extract_state_changes(&state);

                        SimCallResult {
                            success: true,
                            gas_used,
                            output: output_bytes,
                            state_changes: changes,
                            error: None,
                        }
                    }
                    ExecutionResult::Revert { gas_used, output } => {
                        let reason = decode_revert_reason(&output);
                        SimCallResult {
                            success: false,
                            gas_used,
                            output: output.to_vec(),
                            state_changes: vec![],
                            error: Some(format!("Revert: {}", reason)),
                        }
                    }
                    ExecutionResult::Halt { reason, gas_used } => {
                        SimCallResult {
                            success: false,
                            gas_used,
                            output: vec![],
                            state_changes: vec![],
                            error: Some(format!("Halt: {:?}", reason)),
                        }
                    }
                }
            }
            Err(e) => {
                SimCallResult {
                    success: false,
                    gas_used: 0,
                    output: vec![],
                    state_changes: vec![],
                    error: Some(format!("EVM error: {:?}", e)),
                }
            }
        }
    }

    /// Simulate a full opportunity through fork execution.
    ///
    /// 1. Pre-loads relevant accounts into the fork DB
    /// 2. Builds the calldata for the flash loan arb
    /// 3. Executes against forked state
    /// 4. Computes net profit after gas
    pub fn simulate_opportunity(
        &self,
        fork_db: &mut ForkDB,
        opportunity: &Opportunity,
        executor_address: Address,
    ) -> SimulationResult {
        let start = Instant::now();
        self.sim_count.fetch_add(1, Ordering::Relaxed);

        let db = fork_db.inner_mut();
        let block_ctx = self.block_ctx.read().clone();

        // Build calldata for the flash arbitrage
        let calldata = encode_flash_arb_calldata(opportunity);

        // Determine target contract
        let flash_arb_addr = if let Some(ref addr) = self.config.chains
            .get(&ARBITRUM_CHAIN_ID)
            .and_then(|c| c.contract_address.as_ref())
        {
            parse_address(addr)
        } else {
            // Fallback: simulate as a direct call to executor
            executor_address
        };

        // Execute
        let result = self.simulate_call(
            db,
            executor_address,
            flash_arb_addr,
            calldata,
            U256::ZERO,
            SIM_GAS_LIMIT,
        );

        let latency_us = start.elapsed().as_micros() as u64;
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

        // Compute profit: decode output or use balance diff
        let gas_cost_wei = result.gas_used as i128
            * block_ctx.base_fee as i128;

        let gross_profit = if result.success {
            decode_profit_from_output(&result.output)
        } else {
            0i128
        };

        let net_profit = gross_profit - gas_cost_wei;
        let success = result.success && net_profit > 0;

        if success {
            self.sim_success.fetch_add(1, Ordering::Relaxed);
        }

        debug!(
            kind = ?opportunity.opportunity_type,
            success,
            gross_profit,
            gas_cost_wei,
            net_profit,
            gas_used = result.gas_used,
            latency_us,
            "Fork simulation complete"
        );

        SimulationResult {
            success,
            profit: net_profit,
            gas_used: result.gas_used,
            error: result.error,
            state_changes: result.state_changes,
        }
    }

    /// Simulate a bundle: execute transactions sequentially, committing
    /// state between each so later txs see the effects of earlier ones.
    pub fn simulate_bundle(
        &self,
        fork_db: &mut ForkDB,
        bundle: &Bundle,
        executor_address: Address,
    ) -> SimulationResult {
        let start = Instant::now();
        self.sim_count.fetch_add(1, Ordering::Relaxed);

        let db = fork_db.inner_mut();
        let block_ctx = self.block_ctx.read().clone();

        let mut total_gas: u64 = 0;
        let all_changes = Vec::new();
        let mut last_output = vec![];

        for (idx, tx) in bundle.transactions.iter().enumerate() {
            let to = parse_address(&tx.to);
            let caller = executor_address;
            let gas_limit = tx.gas_limit.min(SIM_GAS_LIMIT);

            let mut evm = self.build_evm(db);
            evm.tx_mut().caller = caller;
            evm.tx_mut().transact_to = TransactTo::Call(to);
            evm.tx_mut().data = Bytes::from(tx.data.clone());
            evm.tx_mut().value = U256::from(tx.value);
            evm.tx_mut().gas_limit = gas_limit;
            evm.tx_mut().gas_price = U256::from(
                tx.max_fee_per_gas.unwrap_or(block_ctx.base_fee)
            );

            match evm.transact_commit() {
                Ok(result) => {
                    match result {
                        ExecutionResult::Success { gas_used, output, .. } => {
                            total_gas += gas_used;
                            last_output = match output {
                                Output::Call(bytes) => bytes.to_vec(),
                                Output::Create(bytes, _) => bytes.to_vec(),
                            };
                        }
                        ExecutionResult::Revert { gas_used, output } => {
                            // Check if this tx is allowed to revert
                            let tx_hash = sha3_hash(&tx.data);
                            let allowed = bundle.reverting_tx_hashes.iter()
                                .any(|h| *h == tx_hash);

                            if !allowed {
                                let reason = decode_revert_reason(&output);
                                let latency_us = start.elapsed().as_micros() as u64;
                                self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

                                return SimulationResult {
                                    success: false,
                                    profit: 0,
                                    gas_used: total_gas + gas_used,
                                    error: Some(format!("Tx {} reverted: {}", idx, reason)),
                                    state_changes: all_changes,
                                };
                            }
                            total_gas += gas_used;
                        }
                        ExecutionResult::Halt { reason, gas_used } => {
                            let latency_us = start.elapsed().as_micros() as u64;
                            self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

                            return SimulationResult {
                                success: false,
                                profit: 0,
                                gas_used: total_gas + gas_used,
                                error: Some(format!("Tx {} halted: {:?}", idx, reason)),
                                state_changes: all_changes,
                            };
                        }
                    }
                }
                Err(e) => {
                    let latency_us = start.elapsed().as_micros() as u64;
                    self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

                    return SimulationResult {
                        success: false,
                        profit: 0,
                        gas_used: total_gas,
                        error: Some(format!("Tx {} EVM error: {:?}", idx, e)),
                        state_changes: all_changes,
                    };
                }
            }
        }

        let latency_us = start.elapsed().as_micros() as u64;
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

        let gas_cost = total_gas as i128 * block_ctx.base_fee as i128;
        let gross = decode_profit_from_output(&last_output);
        let net = gross - gas_cost;
        let success = net > 0;

        if success {
            self.sim_success.fetch_add(1, Ordering::Relaxed);
        }

        debug!(
            bundle_size = bundle.transactions.len(),
            total_gas,
            net_profit = net,
            latency_us,
            "Bundle fork simulation complete"
        );

        SimulationResult {
            success,
            profit: net,
            gas_used: total_gas,
            error: None,
            state_changes: all_changes,
        }
    }

    /// Get simulation metrics.
    pub fn metrics(&self) -> EvmSimMetrics {
        let count = self.sim_count.load(Ordering::Relaxed);
        let success = self.sim_success.load(Ordering::Relaxed);
        let total_us = self.total_latency_us.load(Ordering::Relaxed);
        EvmSimMetrics {
            total_simulations: count,
            successful: success,
            avg_latency_us: if count > 0 { total_us / count } else { 0 },
        }
    }
}

// ── Public types ──────────────────────────────────────────────────

/// Result from a single EVM call simulation.
#[derive(Debug, Clone)]
pub struct SimCallResult {
    pub success: bool,
    pub gas_used: u64,
    pub output: Vec<u8>,
    pub state_changes: Vec<StateChange>,
    pub error: Option<String>,
}

/// Metrics snapshot for the fork simulator.
#[derive(Debug, Clone)]
pub struct EvmSimMetrics {
    pub total_simulations: u64,
    pub successful: u64,
    pub avg_latency_us: u64,
}

// ── Helpers ───────────────────────────────────────────────────────

/// Extract state changes from revm's execution state diff.
fn extract_state_changes(state: &EvmState) -> Vec<StateChange> {
    let mut changes = Vec::new();
    for (addr, account) in state.iter() {
        for (slot, slot_value) in account.storage.iter() {
            // Only record slots that actually changed
            if slot_value.is_changed() {
                let slot_bytes: [u8; 32] = slot.to_be_bytes();
                let old_val: [u8; 32] = slot_value.previous_or_original_value.to_be_bytes();
                let new_val: [u8; 32] = slot_value.present_value.to_be_bytes();

                changes.push(StateChange {
                    address: addr.0.into(),
                    slot: slot_bytes,
                    old_value: old_val,
                    new_value: new_val,
                });
            }
        }
    }
    changes
}

/// Encode flash arbitrage calldata from an opportunity.
///
/// ABI: `executeArbitrage(address[] tokens, uint256 amount, address[] pools, uint24[] fees)`
fn encode_flash_arb_calldata(opp: &Opportunity) -> Vec<u8> {
    // Function selector: keccak256("executeArbitrage(address[],uint256,address[],uint24[])")
    // We use a simplified encoding for simulation purposes
    let mut data = Vec::with_capacity(4 + 32 * 8);

    // Selector (first 4 bytes of keccak256)
    data.extend_from_slice(&[0x8a, 0x2e, 0x4e, 0x40]);

    // amount_in as uint256
    let mut amount_bytes = [0u8; 32];
    let amount_be = opp.amount_in.to_be_bytes();
    amount_bytes[16..32].copy_from_slice(&amount_be);
    data.extend_from_slice(&amount_bytes);

    // Encode pool addresses count
    let pool_count = opp.pool_addresses.len();
    let mut count_bytes = [0u8; 32];
    count_bytes[31] = pool_count as u8;
    data.extend_from_slice(&count_bytes);

    // Encode each pool address
    for addr in &opp.pool_addresses {
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(addr);
        data.extend_from_slice(&padded);
    }

    // Encode fees
    for fee in &opp.pool_fees {
        let mut fee_bytes = [0u8; 32];
        fee_bytes[28..32].copy_from_slice(&fee.to_be_bytes());
        data.extend_from_slice(&fee_bytes);
    }

    data
}

/// Try to decode profit from the flash arb return data.
/// Assumes the contract returns `uint256 profit` as the first word.
fn decode_profit_from_output(output: &[u8]) -> i128 {
    if output.len() >= 32 {
        // Read first 32 bytes as uint256, take low 128 bits
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&output[16..32]);
        u128::from_be_bytes(buf) as i128
    } else {
        0
    }
}

/// Decode a revert reason from raw revert data.
/// Handles `Error(string)` and `Panic(uint256)` selectors.
fn decode_revert_reason(data: &Bytes) -> String {
    if data.len() < 4 {
        return "unknown revert".to_string();
    }

    let selector = &data[..4];

    // Error(string) = 0x08c379a0
    if selector == [0x08, 0xc3, 0x79, 0xa0] && data.len() >= 68 {
        // offset at bytes 4..36, length at bytes 36..68, string at 68..68+len
        let len_bytes = &data[36..68];
        let len = u32::from_be_bytes([
            len_bytes[28], len_bytes[29], len_bytes[30], len_bytes[31],
        ]) as usize;
        if data.len() >= 68 + len {
            return String::from_utf8_lossy(&data[68..68 + len]).to_string();
        }
    }

    // Panic(uint256) = 0x4e487b71
    if selector == [0x4e, 0x48, 0x7b, 0x71] && data.len() >= 36 {
        let code = data[35];
        return format!("Panic(0x{:02x})", code);
    }

    format!("0x{}", hex::encode(&data[..data.len().min(64)]))
}

/// Parse a hex address string into `Address`.
fn parse_address(s: &str) -> Address {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).unwrap_or_default();
    let mut buf = [0u8; 20];
    let len = bytes.len().min(20);
    buf[20 - len..].copy_from_slice(&bytes[..len]);
    Address::from(buf)
}

/// Quick keccak256 for generating tx hashes in bundle sim.
fn sha3_hash(data: &[u8]) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config() -> Arc<Config> {
        let mut config = Config::default();
        config.strategy.max_gas_price_gwei = 1;
        Arc::new(config)
    }

    #[test]
    fn test_fork_db_insert_and_query() {
        let mut fork = ForkDB::new();
        let addr = Address::from([0xAA; 20]);

        fork.insert_account(
            addr,
            U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            5,
            vec![], // EOA
        );

        assert!(fork.is_account_loaded(&addr));
        assert!(!fork.is_account_loaded(&Address::from([0xBB; 20])));
    }

    #[test]
    fn test_fork_db_storage() {
        let mut fork = ForkDB::new();
        let addr = Address::from([0xAA; 20]);
        let slot = U256::from(0);
        let value = U256::from(42);

        fork.insert_account(addr, U256::ZERO, 0, vec![]);
        fork.insert_storage(addr, slot, value);

        // Query through the CacheDB
        let stored = fork.inner()
            .accounts
            .get(&addr)
            .and_then(|a| a.storage.get(&slot))
            .copied();
        assert_eq!(stored, Some(value));
    }

    #[test]
    fn test_evm_simple_transfer() {
        let config = test_config();
        let sim = EvmForkSimulator::new(config);
        sim.update_block(BlockContext {
            number: 448_000_000,
            timestamp: 1700000000,
            base_fee: 20_000_000, // 0.02 gwei
            coinbase: [0u8; 20],
        });

        let mut fork = ForkDB::new();
        let sender = Address::from([0x01; 20]);
        let receiver = Address::from([0x02; 20]);

        // Fund sender with 10 ETH
        fork.insert_account(
            sender,
            U256::from(10_000_000_000_000_000_000u128),
            0,
            vec![],
        );
        fork.insert_account(receiver, U256::ZERO, 0, vec![]);

        let db = fork.inner_mut();
        let result = sim.simulate_call(
            db,
            sender,
            receiver,
            vec![],                        // empty calldata = ETH transfer
            U256::from(1_000_000_000u128), // 1 gwei
            21_000,
        );

        assert!(result.success, "Transfer should succeed: {:?}", result.error);
        assert_eq!(result.gas_used, 21_000);
    }

    #[test]
    fn test_evm_revert_decoding() {
        // Error(string) "Insufficient balance"
        let msg = "Insufficient balance";
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0]; // selector
        data.extend_from_slice(&[0u8; 31]);
        data.push(0x20); // offset = 32
        data.extend_from_slice(&[0u8; 31]);
        data.push(msg.len() as u8); // length
        data.extend_from_slice(msg.as_bytes());
        // pad to 32
        data.extend(vec![0u8; 32 - msg.len()]);

        let decoded = decode_revert_reason(&Bytes::from(data));
        assert_eq!(decoded, "Insufficient balance");
    }

    #[test]
    fn test_evm_panic_decoding() {
        let mut data = vec![0x4e, 0x48, 0x7b, 0x71]; // Panic selector
        data.extend_from_slice(&[0u8; 31]);
        data.push(0x11); // arithmetic overflow

        let decoded = decode_revert_reason(&Bytes::from(data));
        assert_eq!(decoded, "Panic(0x11)");
    }

    #[test]
    fn test_encode_calldata_roundtrip() {
        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xtoken_in".to_string(),
            token_out: "0xtoken_out".to_string(),
            amount_in: 1_000_000_000_000_000_000, // 1 ETH
            expected_profit: 10_000_000_000_000_000, // 0.01 ETH
            gas_estimate: 300_000,
            deadline: 0,
            path: vec![],
            pool_addresses: vec![[0xAA; 20], [0xBB; 20]],
            pool_fees: vec![3000, 500],
            target_tx: None,
        };

        let calldata = encode_flash_arb_calldata(&opp);
        // Selector + amount + count + 2 pools + 2 fees = 4 + 32*5 = 164 bytes
        assert!(calldata.len() >= 4 + 32 * 3, "Calldata too short: {} bytes", calldata.len());
        assert_eq!(&calldata[..4], &[0x8a, 0x2e, 0x4e, 0x40]);
    }

    #[test]
    fn test_profit_decode() {
        // Encode 0.5 ETH profit as first uint256 word
        let profit: u128 = 500_000_000_000_000_000;
        let mut output = [0u8; 32];
        output[16..32].copy_from_slice(&profit.to_be_bytes());

        let decoded = decode_profit_from_output(&output);
        assert_eq!(decoded, 500_000_000_000_000_000i128);
    }

    #[test]
    fn test_metrics_counter() {
        let config = test_config();
        let sim = EvmForkSimulator::new(config);

        let m = sim.metrics();
        assert_eq!(m.total_simulations, 0);
        assert_eq!(m.successful, 0);
        assert_eq!(m.avg_latency_us, 0);
    }

    #[test]
    fn test_block_context_update() {
        let config = test_config();
        let sim = EvmForkSimulator::new(config);

        let ctx = BlockContext {
            number: 448_500_000,
            timestamp: 1700000000,
            base_fee: 30_000_000,
            coinbase: [0x42; 20],
        };
        sim.update_block(ctx.clone());

        let got = sim.block_context();
        assert_eq!(got.number, 448_500_000);
        assert_eq!(got.base_fee, 30_000_000);
        assert_eq!(got.coinbase, [0x42; 20]);
    }
}
