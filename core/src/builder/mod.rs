//! Bundle Builder — constructs Flashbots-compatible bundles from opportunities
//!
//! Converts detected MEV opportunities into signed transaction bundles
//! ready for relay submission. Each opportunity type has a bespoke
//! encoding strategy targeting the on-chain executor contracts.

use crate::config::Config;
use crate::types::{Bundle, BundleTransaction, Opportunity, OpportunityType, DexType};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, debug};

/// Solidity function selectors for executor contract
mod selectors {
    /// executeArbitrage(address token, uint256 amount, bytes swapPath)
    pub const EXECUTE_ARBITRAGE: [u8; 4] = [0xa0, 0x71, 0x2d, 0x68];
    /// executeBackrun(address token, uint256 amount, bytes swapData)
    pub const EXECUTE_BACKRUN: [u8; 4] = [0x5a, 0xf0, 0x6f, 0xed];
    /// executeLiquidation(address debtToken, address collToken, uint256 amount, address user)
    pub const EXECUTE_LIQUIDATION: [u8; 4] = [0xd4, 0xe8, 0xbe, 0x83];
}

/// DEX type identifiers used in swap path encoding
const DEX_UNISWAP_V2: u8 = 0x01;
const DEX_UNISWAP_V3: u8 = 0x02;
const DEX_SUSHISWAP: u8  = 0x03;
const DEX_CURVE: u8       = 0x04;
const DEX_BALANCER: u8    = 0x05;

/// Bundle builder for MEV extraction
pub struct BundleBuilder {
    #[allow(dead_code)]
    config: Arc<Config>,
    count: AtomicU64,
    contract_address: Option<String>,
}

impl BundleBuilder {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            count: AtomicU64::new(0),
            contract_address: None,
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Bundle builder started");
        Ok(())
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        let built = self.count.load(Ordering::Relaxed);
        info!(bundles_built = built, "Bundle builder stopped");
        Ok(())
    }

    /// Set the on-chain executor contract address
    pub fn set_contract(&mut self, address: String) {
        self.contract_address = Some(address);
    }

    /// Build a bundle from a detected opportunity
    pub async fn build(&self, opportunity: &Opportunity) -> anyhow::Result<Bundle> {
        let contract = self.contract_address.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Contract address not set"))?;

        let transactions = match opportunity.opportunity_type {
            OpportunityType::Arbitrage => self.build_arbitrage_bundle(opportunity, contract)?,
            OpportunityType::Backrun => self.build_backrun_bundle(opportunity, contract)?,
            OpportunityType::Liquidation => self.build_liquidation_bundle(opportunity, contract)?,
        };

        self.count.fetch_add(1, Ordering::Relaxed);

        debug!(
            kind = ?opportunity.opportunity_type,
            txs = transactions.len(),
            gas = opportunity.gas_estimate,
            "Bundle built"
        );

        Ok(Bundle {
            transactions,
            target_block: None,
            max_block_number: None,
            min_timestamp: None,
            max_timestamp: None,
            reverting_tx_hashes: vec![],
        })
    }

    /// Arbitrage bundle: single flash-loan tx that executes the full arb path
    fn build_arbitrage_bundle(
        &self,
        opportunity: &Opportunity,
        contract: &str,
    ) -> anyhow::Result<Vec<BundleTransaction>> {
        self.validate_swap_opportunity(opportunity)?;
        let swap_path = self.encode_swap_path(opportunity)?;

        let calldata = encode_arbitrage_call(
            &opportunity.token_in,
            opportunity.amount_in,
            &swap_path,
        )?;

        Ok(vec![BundleTransaction {
            to: contract.to_string(),
            value: 0,
            gas_limit: opportunity.gas_estimate,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: Some(1_000_000_000), // 1 gwei tip
            data: calldata,
            nonce: None,
        }])
    }

    /// Backrun bundle: execute immediately after target tx
    fn build_backrun_bundle(
        &self,
        opportunity: &Opportunity,
        contract: &str,
    ) -> anyhow::Result<Vec<BundleTransaction>> {
        self.validate_swap_opportunity(opportunity)?;
        let swap_path = self.encode_swap_path(opportunity)?;

        let calldata = encode_backrun_call(
            &opportunity.token_in,
            opportunity.amount_in,
            &swap_path,
        )?;

        Ok(vec![BundleTransaction {
            to: contract.to_string(),
            value: 0,
            gas_limit: opportunity.gas_estimate,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: Some(2_000_000_000), // 2 gwei — higher for backrun priority
            data: calldata,
            nonce: None,
        }])
    }

    /// Liquidation bundle: flash borrow → repay debt → receive collateral
    fn build_liquidation_bundle(
        &self,
        opportunity: &Opportunity,
        contract: &str,
    ) -> anyhow::Result<Vec<BundleTransaction>> {
        let calldata = encode_liquidation_call(
            &opportunity.token_in,   // debt token
            &opportunity.token_out,  // collateral token
            opportunity.amount_in,
        )?;

        Ok(vec![BundleTransaction {
            to: contract.to_string(),
            value: 0,
            gas_limit: opportunity.gas_estimate,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: Some(1_000_000_000),
            data: calldata,
            nonce: None,
        }])
    }

    /// Encode the multi-hop swap path for the executor contract
    ///
    /// Wire format per hop: [dex_type:1][pool_address:20][fee:3]
    fn encode_swap_path(&self, opportunity: &Opportunity) -> anyhow::Result<Vec<u8>> {
        self.validate_swap_opportunity(opportunity)?;

        let path = &opportunity.path;
        let pool_addresses = &opportunity.pool_addresses;
        let pool_fees = &opportunity.pool_fees;

        let mut data = Vec::with_capacity(path.len() * 24);

        for (i, dex) in path.iter().enumerate() {
            let dex_byte = match dex {
                DexType::UniswapV2  => DEX_UNISWAP_V2,
                DexType::UniswapV3  => DEX_UNISWAP_V3,
                DexType::SushiSwap  => DEX_SUSHISWAP,
                DexType::Curve      => DEX_CURVE,
                DexType::Balancer   => DEX_BALANCER,
            };

            // Safe due to validate_swap_opportunity() length checks.
            let pool_addr = pool_addresses[i];

            // Use per-hop fee if available, otherwise derive from DEX type
            let fee = pool_fees.get(i).copied().unwrap_or(default_fee_for_dex(dex));
            let fee_bytes = [
                ((fee >> 16) & 0xFF) as u8,
                ((fee >> 8) & 0xFF) as u8,
                (fee & 0xFF) as u8,
            ];

            data.push(dex_byte);
            data.extend_from_slice(&pool_addr);
            data.extend_from_slice(&fee_bytes);
        }

        Ok(data)
    }

    /// Validate path metadata for swap-based opportunities (arb/backrun).
    fn validate_swap_opportunity(&self, opportunity: &Opportunity) -> anyhow::Result<()> {
        let path = &opportunity.path;
        let pool_addresses = &opportunity.pool_addresses;
        let pool_fees = &opportunity.pool_fees;

        if path.is_empty() {
            return Err(anyhow::anyhow!("swap path is empty"));
        }

        if pool_addresses.len() != path.len() {
            return Err(anyhow::anyhow!(
                "pool address count mismatch: path has {} hops, pool_addresses has {} entries",
                path.len(),
                pool_addresses.len()
            ));
        }

        if !pool_fees.is_empty() && pool_fees.len() != path.len() {
            return Err(anyhow::anyhow!(
                "pool fee count mismatch: path has {} hops, pool_fees has {} entries",
                path.len(),
                pool_fees.len()
            ));
        }

        for (i, dex) in path.iter().enumerate() {
            let pool_addr = pool_addresses[i];
            if pool_addr == [0u8; 20] {
                return Err(anyhow::anyhow!(
                    "pool address not resolved for hop {} ({:?})",
                    i,
                    dex
                ));
            }

            let fee = pool_fees.get(i).copied().unwrap_or(default_fee_for_dex(dex));
            if !is_valid_fee_for_dex(dex, fee) {
                return Err(anyhow::anyhow!(
                    "invalid fee {} for hop {} ({:?})",
                    fee,
                    i,
                    dex
                ));
            }
        }

        Ok(())
    }

    pub async fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

// ─── ABI encoding helpers ──────────────────────────────────────

/// Encode executeArbitrage(address token, uint256 amount, bytes swapPath)
fn encode_arbitrage_call(
    token: &str,
    amount: u128,
    swap_data: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut calldata = Vec::with_capacity(4 + 32 * 4 + swap_data.len());

    // Selector
    calldata.extend_from_slice(&selectors::EXECUTE_ARBITRAGE);

    // Param 1: address token (left-padded to 32 bytes)
    calldata.extend_from_slice(&abi_encode_address(token));

    // Param 2: uint256 amount
    calldata.extend_from_slice(&abi_encode_u256(amount));

    // Param 3: bytes offset (dynamic — points past fixed params)
    calldata.extend_from_slice(&abi_encode_u256(96)); // 3 * 32

    // Dynamic: length + data
    calldata.extend_from_slice(&abi_encode_u256(swap_data.len() as u128));
    calldata.extend_from_slice(swap_data);
    // Pad to 32-byte boundary
    let pad = (32 - swap_data.len() % 32) % 32;
    calldata.extend(std::iter::repeat(0u8).take(pad));

    Ok(calldata)
}

/// Encode executeBackrun(address token, uint256 amount, bytes swapData)
fn encode_backrun_call(
    token: &str,
    amount: u128,
    swap_data: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut calldata = Vec::with_capacity(4 + 32 * 4 + swap_data.len());

    calldata.extend_from_slice(&selectors::EXECUTE_BACKRUN);
    calldata.extend_from_slice(&abi_encode_address(token));
    calldata.extend_from_slice(&abi_encode_u256(amount));
    calldata.extend_from_slice(&abi_encode_u256(96));
    calldata.extend_from_slice(&abi_encode_u256(swap_data.len() as u128));
    calldata.extend_from_slice(swap_data);
    let pad = (32 - swap_data.len() % 32) % 32;
    calldata.extend(std::iter::repeat(0u8).take(pad));

    Ok(calldata)
}

/// Encode executeLiquidation(address debtToken, address collToken, uint256 amount, address user)
fn encode_liquidation_call(
    debt_token: &str,
    collateral_token: &str,
    amount: u128,
) -> anyhow::Result<Vec<u8>> {
    let mut calldata = Vec::with_capacity(4 + 32 * 4);

    calldata.extend_from_slice(&selectors::EXECUTE_LIQUIDATION);
    calldata.extend_from_slice(&abi_encode_address(debt_token));
    calldata.extend_from_slice(&abi_encode_address(collateral_token));
    calldata.extend_from_slice(&abi_encode_u256(amount));
    // User address (zero = liquidate any matching position)
    calldata.extend_from_slice(&[0u8; 32]);

    Ok(calldata)
}

/// ABI-encode an address into a 32-byte word (left-padded with zeros)
fn abi_encode_address(addr: &str) -> [u8; 32] {
    let mut word = [0u8; 32];
    let hex_str = addr.strip_prefix("0x").unwrap_or(addr);
    if let Ok(bytes) = hex::decode(hex_str) {
        let len = bytes.len().min(20);
        word[32 - len..32].copy_from_slice(&bytes[..len]);
    }
    word
}

/// ABI-encode a u128 into a 32-byte word (big-endian, left-padded)
fn abi_encode_u256(val: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..32].copy_from_slice(&val.to_be_bytes());
    word
}

fn default_fee_for_dex(dex: &DexType) -> u32 {
    match dex {
        DexType::UniswapV2 => 3000,
        DexType::UniswapV3 => 500,
        DexType::SushiSwap => 3000,
        DexType::Curve => 4,
        DexType::Balancer => 0,
    }
}

fn is_valid_fee_for_dex(dex: &DexType, fee: u32) -> bool {
    match dex {
        DexType::UniswapV2 | DexType::SushiSwap => fee == 3000,
        DexType::UniswapV3 => matches!(fee, 100 | 500 | 3000 | 10000),
        DexType::Curve => (1..=10_000).contains(&fee),
        DexType::Balancer => fee <= 10_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_abi_encode_address() {
        let addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"; // WETH
        let encoded = abi_encode_address(addr);
        // First 12 bytes should be zero padding
        assert_eq!(&encoded[..12], &[0u8; 12]);
        // Last 20 bytes should be the address
        assert_eq!(encoded[12], 0xC0);
    }

    #[test]
    fn test_abi_encode_u256() {
        let val = 1_000_000_000_000_000_000u128; // 1e18
        let encoded = abi_encode_u256(val);
        assert_eq!(&encoded[..16], &[0u8; 16]); // first 16 bytes zero
        assert_eq!(
            u128::from_be_bytes(encoded[16..32].try_into().unwrap()),
            val
        );
    }

    #[test]
    fn test_encode_arbitrage_call() {
        let swap_data = {
            let mut v = vec![DEX_UNISWAP_V2];
            v.extend_from_slice(&[0x00; 23]);
            v
        }; // 24-byte path
        let calldata = encode_arbitrage_call(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            1_000_000_000_000_000_000,
            &swap_data,
        ).unwrap();

        // Check selector
        assert_eq!(&calldata[0..4], &selectors::EXECUTE_ARBITRAGE);
        // Check total length: 4 + 32*3 (fixed) + 32 (len) + 32 (padded data) = 164
        assert!(calldata.len() >= 164);
    }

    #[test]
    fn test_encode_liquidation_call() {
        let calldata = encode_liquidation_call(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", // USDC
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", // WETH
            50_000_000_000_000_000_000,
        ).unwrap();

        assert_eq!(&calldata[0..4], &selectors::EXECUTE_LIQUIDATION);
        assert_eq!(calldata.len(), 4 + 32 * 4); // fixed params only
    }

    #[tokio::test]
    async fn test_build_arbitrage_bundle() {
        let config = Arc::new(Config::default());
        let mut builder = BundleBuilder::new(config);
        builder.set_contract("0x1234567890123456789012345678901234567890".to_string());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 5_000_000_000_000_000_000,
            expected_profit: 50_000_000_000_000_000,
            gas_estimate: 250_000,
            deadline: 100,
            path: vec![DexType::UniswapV2, DexType::UniswapV3],
            pool_addresses: vec![[0xAA; 20], [0xBB; 20]],
            pool_fees: vec![3000, 500],
            target_tx: None,
        };

        let bundle = builder.build(&opp).await.unwrap();
        assert_eq!(bundle.transactions.len(), 1);
        assert_eq!(bundle.transactions[0].gas_limit, 250_000);
        assert!(!bundle.transactions[0].data.is_empty());
    }

    #[test]
    fn test_encode_swap_path() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 10_000_000_000_000_000,
            gas_estimate: 250_000,
            deadline: 100,
            path: vec![DexType::UniswapV2, DexType::UniswapV3],
            pool_addresses: vec![[0xAA; 20], [0xBB; 20]],
            pool_fees: vec![3000, 500],
            target_tx: None,
        };
        let encoded = builder.encode_swap_path(&opp).unwrap();

        // 2 hops × 24 bytes each = 48
        assert_eq!(encoded.len(), 48);
        assert_eq!(encoded[0], DEX_UNISWAP_V2);
        // Pool address should be real, not zeros
        assert_eq!(encoded[1..21], [0xAA; 20]);
        assert_eq!(encoded[24], DEX_UNISWAP_V3);
        assert_eq!(encoded[25..45], [0xBB; 20]);
    }

    #[tokio::test]
    async fn test_build_backrun_bundle() {
        let config = Arc::new(Config::default());
        let mut builder = BundleBuilder::new(config);
        builder.set_contract("0x1234567890123456789012345678901234567890".to_string());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Backrun,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 2_000_000_000_000_000_000,
            expected_profit: 20_000_000_000_000_000,
            gas_estimate: 200_000,
            deadline: 50,
            path: vec![DexType::UniswapV3],
            pool_addresses: vec![[0xCC; 20]],
            pool_fees: vec![500],
            target_tx: None,
        };

        let bundle = builder.build(&opp).await.unwrap();
        assert_eq!(bundle.transactions.len(), 1);
        // Backrun should use EXECUTE_BACKRUN selector
        assert_eq!(&bundle.transactions[0].data[0..4], &selectors::EXECUTE_BACKRUN);
        // Priority fee is 2 gwei for backrun
        assert_eq!(bundle.transactions[0].max_priority_fee_per_gas, Some(2_000_000_000));
    }

    #[tokio::test]
    async fn test_build_liquidation_bundle() {
        let config = Arc::new(Config::default());
        let mut builder = BundleBuilder::new(config);
        builder.set_contract("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Liquidation,
            token_in: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token_out: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            amount_in: 50_000_000_000_000_000_000,
            expected_profit: 5_000_000_000_000_000_000,
            gas_estimate: 450_000,
            deadline: 0,
            path: vec![],
            pool_addresses: vec![],
            pool_fees: vec![],
            target_tx: None,
        };

        let bundle = builder.build(&opp).await.unwrap();
        assert_eq!(bundle.transactions.len(), 1);
        assert_eq!(&bundle.transactions[0].data[0..4], &selectors::EXECUTE_LIQUIDATION);
        assert_eq!(bundle.transactions[0].gas_limit, 450_000);
    }

    #[tokio::test]
    async fn test_build_without_contract_fails() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 10_000_000_000_000_000,
            gas_estimate: 250_000,
            deadline: 100,
            path: vec![DexType::UniswapV2],
            pool_addresses: vec![[0xAA; 20]],
            pool_fees: vec![3000],
            target_tx: None,
        };

        let result = builder.build(&opp).await;
        assert!(result.is_err(), "build without contract should fail");
    }

    #[tokio::test]
    async fn test_build_count_increments() {
        let config = Arc::new(Config::default());
        let mut builder = BundleBuilder::new(config);
        builder.set_contract("0x1234567890123456789012345678901234567890".to_string());

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token_out: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 10_000_000_000_000_000,
            gas_estimate: 250_000,
            deadline: 100,
            path: vec![DexType::UniswapV2],
            pool_addresses: vec![[0xAA; 20]],
            pool_fees: vec![3000],
            target_tx: None,
        };

        assert_eq!(builder.count().await, 0);
        builder.build(&opp).await.unwrap();
        assert_eq!(builder.count().await, 1);
        builder.build(&opp).await.unwrap();
        assert_eq!(builder.count().await, 2);
    }

    #[test]
    fn test_encode_swap_path_empty_fails() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xdead".to_string(),
            token_out: "0xbeef".to_string(),
            amount_in: 0,
            expected_profit: 0,
            gas_estimate: 100_000,
            deadline: 0,
            path: vec![],
            pool_addresses: vec![],
            pool_fees: vec![],
            target_tx: None,
        };
        let result = builder.encode_swap_path(&opp);
        assert!(result.is_err(), "empty path must fail validation");
    }

    #[test]
    fn test_encode_swap_path_missing_pool_fails() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xdead".to_string(),
            token_out: "0xbeef".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 0,
            gas_estimate: 250_000,
            deadline: 0,
            path: vec![DexType::UniswapV2],
            pool_addresses: vec![], // no pool address provided
            pool_fees: vec![],
            target_tx: None,
        };
        let result = builder.encode_swap_path(&opp);
        assert!(result.is_err(), "missing pool metadata must fail validation");
    }

    #[test]
    fn test_encode_swap_path_invalid_v3_fee_fails() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xdead".to_string(),
            token_out: "0xbeef".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 0,
            gas_estimate: 250_000,
            deadline: 0,
            path: vec![DexType::UniswapV3],
            pool_addresses: vec![[0x11; 20]],
            pool_fees: vec![2500],
            target_tx: None,
        };

        let result = builder.encode_swap_path(&opp);
        assert!(result.is_err(), "invalid UniswapV3 fee tier must fail validation");
    }

    #[test]
    fn test_encode_swap_path_fee_count_mismatch_fails() {
        let config = Arc::new(Config::default());
        let builder = BundleBuilder::new(config);

        let opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            token_in: "0xdead".to_string(),
            token_out: "0xbeef".to_string(),
            amount_in: 1_000_000_000_000_000_000,
            expected_profit: 0,
            gas_estimate: 250_000,
            deadline: 0,
            path: vec![DexType::UniswapV2, DexType::UniswapV3],
            pool_addresses: vec![[0xAA; 20], [0xBB; 20]],
            pool_fees: vec![3000],
            target_tx: None,
        };

        let result = builder.encode_swap_path(&opp);
        assert!(result.is_err(), "pool fee count mismatch must fail validation");
    }

    #[test]
    fn test_abi_encode_address_no_prefix() {
        let addr = "C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"; // no 0x
        let encoded = abi_encode_address(addr);
        assert_eq!(&encoded[..12], &[0u8; 12]);
        assert_eq!(encoded[12], 0xC0);
    }

    #[test]
    fn test_abi_encode_address_empty() {
        let encoded = abi_encode_address("");
        assert_eq!(encoded, [0u8; 32]);
    }

    #[test]
    fn test_abi_encode_u256_zero() {
        let encoded = abi_encode_u256(0);
        assert_eq!(encoded, [0u8; 32]);
    }

    #[test]
    fn test_abi_encode_u256_max() {
        let encoded = abi_encode_u256(u128::MAX);
        assert_eq!(&encoded[..16], &[0u8; 16]);
        assert_eq!(
            u128::from_be_bytes(encoded[16..32].try_into().unwrap()),
            u128::MAX
        );
    }
}
