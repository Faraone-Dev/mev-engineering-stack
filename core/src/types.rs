//! Core types for the MEV engine pipeline.
//!
//! These types flow through the entire detection → simulation → bundle path:
//! `PendingTx` → `SwapInfo` → `Opportunity` → `SimulationResult` → `Bundle`

use serde::{Deserialize, Serialize};

/// MEV opportunity classification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpportunityType {
    /// Cross-DEX price discrepancy (buy low, sell high)
    Arbitrage,
    /// Capture price recovery after a large swap
    Backrun,
    /// Repay under-collateralized lending position for bonus
    Liquidation,
}

/// Supported decentralized exchange protocols
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    Curve,
    Balancer,
}

/// Raw pending transaction from the mempool
#[derive(Debug, Clone)]
pub struct PendingTx {
    /// Keccak-256 transaction hash
    pub hash: [u8; 32],
    /// Sender address
    pub from: [u8; 20],
    /// Recipient (None for contract creation)
    pub to: Option<[u8; 20]>,
    /// ETH value in wei
    pub value: u128,
    /// Gas price in wei (legacy) or maxFeePerGas (EIP-1559)
    pub gas_price: u128,
    /// Maximum gas units allowed
    pub gas_limit: u64,
    /// Raw calldata (ABI-encoded function call)
    pub input: Vec<u8>,
    /// Sender nonce
    pub nonce: u64,
    /// Unix timestamp when first seen in mempool
    pub timestamp: u64,
}

/// Decoded swap parameters from calldata
#[derive(Debug, Clone)]
pub struct SwapInfo {
    /// Which DEX protocol this swap targets
    pub dex: DexType,
    /// Input token address (checksummed hex)
    pub token_in: String,
    /// Output token address (checksummed hex)
    pub token_out: String,
    /// Input amount in token's smallest unit
    pub amount_in: u128,
    /// Minimum acceptable output (slippage protection)
    pub amount_out_min: u128,
    /// Pool fee in hundredths of a basis point (e.g., 3000 = 0.30%)
    pub fee: u32,
}

/// Detected MEV opportunity ready for simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    /// Classification of the MEV strategy
    pub opportunity_type: OpportunityType,
    /// Token to acquire / debt token to repay
    pub token_in: String,
    /// Token to sell / collateral token to receive
    pub token_out: String,
    /// Flash loan or swap input amount (wei)
    pub amount_in: u128,
    /// Estimated profit after gas and fees (wei)
    pub expected_profit: u128,
    /// Estimated gas units for the full bundle
    pub gas_estimate: u64,
    /// Block deadline after which opportunity expires
    pub deadline: u64,
    /// Multi-hop swap path (DEX sequence)
    pub path: Vec<DexType>,
    /// Hash of the target tx to backrun (if applicable)
    pub target_tx: Option<[u8; 32]>,
}

/// Result from EVM simulation of an opportunity
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Whether the simulated bundle executed without revert
    pub success: bool,
    /// Net profit in wei (negative = loss)
    pub profit: i128,
    /// Actual gas consumed in simulation
    pub gas_used: u64,
    /// Revert reason or simulation error
    pub error: Option<String>,
    /// Storage slot changes for state-diff validation
    pub state_changes: Vec<StateChange>,
}

/// Single storage slot mutation from simulation
#[derive(Debug, Clone)]
pub struct StateChange {
    /// Contract address whose storage changed
    pub address: [u8; 20],
    /// Storage slot index
    pub slot: [u8; 32],
    /// Value before the simulated bundle
    pub old_value: [u8; 32],
    /// Value after the simulated bundle
    pub new_value: [u8; 32],
}

/// Flashbots-compatible bundle for relay submission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// Ordered transactions in the bundle
    pub transactions: Vec<BundleTransaction>,
    /// Preferred inclusion block
    pub target_block: Option<u64>,
    /// Latest block to attempt inclusion
    pub max_block_number: Option<u64>,
    /// Earliest valid timestamp (MEV timing constraint)
    pub min_timestamp: Option<u64>,
    /// Latest valid timestamp
    pub max_timestamp: Option<u64>,
    /// Tx hashes allowed to revert without failing the bundle
    pub reverting_tx_hashes: Vec<[u8; 32]>,
}

/// Single transaction within a Flashbots bundle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleTransaction {
    /// Recipient contract address (executor)
    pub to: String,
    /// ETH value to send (usually 0 for MEV bundles)
    pub value: u128,
    /// Gas limit for this transaction
    pub gas_limit: u64,
    /// Legacy gas price (mutually exclusive with EIP-1559 fields)
    pub gas_price: Option<u128>,
    /// EIP-1559 max fee per gas
    pub max_fee_per_gas: Option<u128>,
    /// EIP-1559 priority fee (miner tip) — key for bundle ordering
    pub max_priority_fee_per_gas: Option<u128>,
    /// ABI-encoded calldata for the executor contract
    pub data: Vec<u8>,
    /// Explicit nonce (None = use pending nonce)
    pub nonce: Option<u64>,
}

/// Result from submitting a bundle to a relay
#[derive(Debug, Clone)]
pub struct BundleResult {
    /// Bundle hash returned by the relay
    pub bundle_hash: [u8; 32],
    /// Whether the relay accepted the submission
    pub submitted: bool,
    /// Block where the bundle was included (None if pending/dropped)
    pub included_block: Option<u64>,
    /// Error message from relay rejection
    pub error: Option<String>,
}

/// AMM pool state for constant-product simulation
#[derive(Debug, Clone)]
pub struct PoolState {
    /// Pool contract address
    pub address: [u8; 20],
    /// Token0 address (sorted lower)
    pub token0: [u8; 20],
    /// Token1 address (sorted higher)
    pub token1: [u8; 20],
    /// Reserve of token0 in pool's smallest unit
    pub reserve0: u128,
    /// Reserve of token1 in pool's smallest unit
    pub reserve1: u128,
    /// Pool fee in hundredths of a basis point
    pub fee: u32,
}
