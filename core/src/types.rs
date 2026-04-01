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
    /// Pool addresses for each hop in path (parallel to `path`)
    /// Each entry is the on-chain pool contract address for that swap step.
    pub pool_addresses: Vec<[u8; 20]>,
    /// Fee tiers per hop in basis points (parallel to `path`)
    pub pool_fees: Vec<u32>,
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

/// AMM pool state for simulation (V2 constant-product & V3 concentrated liquidity)
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
    /// V3: current sqrt price as Q64.96 fixed-point (0 for V2 pools)
    pub sqrt_price_x96: u128,
    /// V3: active in-range liquidity L (0 for V2 pools)
    pub liquidity: u128,
    /// Whether this is a V3-style concentrated liquidity pool
    pub is_v3: bool,
    /// V3: current tick index (0 for V2 pools)
    pub current_tick: i32,
    /// V3: tick spacing — determines granularity of initialized ticks (0 for V2 pools)
    pub tick_spacing: i32,
    /// V3: initialized tick data sorted by index — `(tick_index, sqrt_price_x96_at_tick, liquidity_net)`.
    /// `sqrt_price_x96_at_tick` is the exact sqrtPriceX96 at this tick boundary.
    /// `liquidity_net` is the signed change in active liquidity when crossing this tick
    /// from left to right. Empty for V2 pools or when tick data is unavailable (falls
    /// back to single-range approximation).
    pub ticks: Vec<(i32, u128, i128)>,
}

// ─── Gas estimation ──────────────────────────────────────────────

/// Base EVM transaction overhead (intrinsic gas).
const BASE_TX_GAS: u64 = 21_000;
/// Flash loan callback overhead (Balancer vault enter/exit).
const FLASH_LOAN_GAS: u64 = 80_000;

/// Per-hop gas cost by DEX type.
fn dex_hop_gas(dex: &DexType) -> u64 {
    match dex {
        DexType::UniswapV2 | DexType::SushiSwap => 120_000,
        DexType::UniswapV3 => 150_000,  // tick traversal
        DexType::Curve => 200_000,       // multi-asset stable math
        DexType::Balancer => 180_000,    // weighted pool calc
    }
}

/// Estimate gas for an opportunity based on path complexity.
///
/// Formula: `BASE_TX + FLASH_LOAN + sum(dex_hop_gas(each_hop))`
///
/// This replaces the old hardcoded constants (250k arb, 180k backrun)
/// and scales correctly for multi-hop paths.
pub fn estimate_gas(opp_type: &OpportunityType, path: &[DexType]) -> u64 {
    let hops: u64 = path.iter().map(dex_hop_gas).sum();
    let flash = match opp_type {
        OpportunityType::Arbitrage | OpportunityType::Liquidation => FLASH_LOAN_GAS,
        OpportunityType::Backrun => 0, // backruns don't use flash loans
    };
    BASE_TX_GAS + flash + hops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_gas_arb_two_v2_hops() {
        // BASE_TX(21k) + FLASH_LOAN(80k) + 2 * UniV2(120k) = 341k
        let gas = estimate_gas(
            &OpportunityType::Arbitrage,
            &[DexType::UniswapV2, DexType::UniswapV2],
        );
        assert_eq!(gas, 341_000);
    }

    #[test]
    fn test_estimate_gas_arb_v2_v3_mixed() {
        // 21k + 80k + 120k (V2) + 150k (V3) = 371k
        let gas = estimate_gas(
            &OpportunityType::Arbitrage,
            &[DexType::UniswapV2, DexType::UniswapV3],
        );
        assert_eq!(gas, 371_000);
    }

    #[test]
    fn test_estimate_gas_backrun_single_v3() {
        // 21k + 0 (no flash) + 150k = 171k
        let gas = estimate_gas(
            &OpportunityType::Backrun,
            &[DexType::UniswapV3],
        );
        assert_eq!(gas, 171_000);
    }

    #[test]
    fn test_estimate_gas_liquidation_empty_path() {
        // 21k + 80k + 0 = 101k (no swap hops, just flash + repay)
        let gas = estimate_gas(&OpportunityType::Liquidation, &[]);
        assert_eq!(gas, 101_000);
    }

    #[test]
    fn test_estimate_gas_curve_hop() {
        // 21k + 80k + 200k = 301k
        let gas = estimate_gas(
            &OpportunityType::Arbitrage,
            &[DexType::Curve],
        );
        assert_eq!(gas, 301_000);
    }

    #[test]
    fn test_estimate_gas_balancer_hop() {
        // 21k + 80k + 180k = 281k
        let gas = estimate_gas(
            &OpportunityType::Arbitrage,
            &[DexType::Balancer],
        );
        assert_eq!(gas, 281_000);
    }

    #[test]
    fn test_estimate_gas_sushi_same_as_v2() {
        let v2 = estimate_gas(&OpportunityType::Arbitrage, &[DexType::UniswapV2]);
        let sushi = estimate_gas(&OpportunityType::Arbitrage, &[DexType::SushiSwap]);
        assert_eq!(v2, sushi);
    }

    #[test]
    fn test_estimate_gas_three_hop_arb() {
        // Triangular arb: V2 → V3 → Sushi
        // 21k + 80k + 120k + 150k + 120k = 491k
        let gas = estimate_gas(
            &OpportunityType::Arbitrage,
            &[DexType::UniswapV2, DexType::UniswapV3, DexType::SushiSwap],
        );
        assert_eq!(gas, 491_000);
    }
}
