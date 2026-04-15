// Arbitrage Executor for Arbitrum
// Builds and submits flash loan arbitrage transactions

use super::detector::{ArbitrageOpportunity, ArbitrageStep};
use super::pools::PoolType;
use ethers::{
    prelude::*,
    types::{Address, Bytes, U256, TransactionRequest},
    utils::keccak256,
};
use std::sync::Arc;

// Flash arbitrage contract interface
abigen!(
    FlashArbitrageContract,
    r#"[
        function executeArbitrage(address[] calldata tokens, uint256[] calldata amounts, bytes calldata userData) external
        function owner() external view returns (address)
    ]"#
);

/// Executor configuration
pub struct ExecutorConfig {
    pub contract_address: Address,
    pub private_key: LocalWallet,
    pub max_gas_price: U256,
    pub priority_fee: U256,
    pub slippage_bps: u32,
}

/// Arbitrage executor
pub struct ArbitrageExecutor {
    provider: Arc<Provider<Http>>,
    config: ExecutorConfig,
    #[allow(dead_code)]
    balancer_vault: Address,
}

impl ArbitrageExecutor {
    pub fn new(
        provider: Arc<Provider<Http>>,
        config: ExecutorConfig,
    ) -> Self {
        use std::str::FromStr;
        
        Self {
            provider,
            config,
            balancer_vault: Address::from_str("0xBA12222222228d8Ba445958a75a0704d566BF2C8").expect("invalid Balancer vault address"),
        }
    }
    
    /// Execute an arbitrage opportunity
    pub async fn execute(&self, opp: &ArbitrageOpportunity) -> Result<TxHash, ExecutorError> {
        // Build calldata for our contract
        let user_data = self.encode_swap_path(&opp.path)?;
        
        // Get tokens and amounts for flash loan
        let tokens = vec![opp.input_token];
        let amounts = vec![opp.input_amount];
        
        // Apply slippage protection
        let _min_output = opp.output_amount * (10000 - self.config.slippage_bps) / 10000;
        
        // Build transaction
        let contract = FlashArbitrageContract::new(
            self.config.contract_address,
            self.provider.clone(),
        );
        
        // Encode function call
        let call = contract.execute_arbitrage(tokens, amounts, user_data.into());
        let tx = call.tx;
        
        // Get current gas price
        let gas_price = self.provider.get_gas_price().await
            .map_err(|e| ExecutorError::Provider(e.to_string()))?;
        
        if gas_price > self.config.max_gas_price {
            return Err(ExecutorError::GasTooHigh(gas_price));
        }
        
        // Sign and send
        let wallet = self.config.private_key.clone()
            .with_chain_id(42161u64); // Arbitrum
        
        let client = SignerMiddleware::new(self.provider.clone(), wallet);
        
        let pending_tx = client.send_transaction(tx, None).await
            .map_err(|e| ExecutorError::Send(e.to_string()))?;
        
        Ok(pending_tx.tx_hash())
    }
    
    /// Simulate execution without sending
    pub async fn simulate(&self, opp: &ArbitrageOpportunity) -> Result<SimulationResult, ExecutorError> {
        let user_data = self.encode_swap_path(&opp.path)?;
        let tokens = vec![opp.input_token];
        let amounts = vec![opp.input_amount];
        
        let contract = FlashArbitrageContract::new(
            self.config.contract_address,
            self.provider.clone(),
        );
        
        let call = contract.execute_arbitrage(tokens, amounts, user_data.into());
        
        // Estimate gas
        let gas_estimate = call.estimate_gas().await
            .map_err(|e| ExecutorError::Simulation(e.to_string()))?;
        
        let gas_price = self.provider.get_gas_price().await
            .map_err(|e| ExecutorError::Provider(e.to_string()))?;
        
        let gas_cost = gas_estimate * gas_price;
        let net_profit = if opp.profit > gas_cost {
            opp.profit - gas_cost
        } else {
            U256::zero()
        };
        
        Ok(SimulationResult {
            success: true,
            gas_estimate,
            gas_cost,
            net_profit,
            error: None,
        })
    }
    
    /// Encode swap path for contract
    fn encode_swap_path(&self, path: &[ArbitrageStep]) -> Result<Vec<u8>, ExecutorError> {
        use ethers::abi::{encode, Token};
        
        // Encode each step
        let mut encoded_steps = Vec::new();
        
        for step in path {
            let dex_type = match step.pool_type {
                PoolType::UniswapV3 { fee } => {
                    // Type 1 = V3, include fee
                    let step_data = encode(&[
                        Token::Uint(U256::from(1)), // DEX type
                        Token::Address(step.pool),
                        Token::Address(step.token_in),
                        Token::Address(step.token_out),
                        Token::Uint(U256::from(fee)),
                        Token::Uint(step.amount_out * 95 / 100), // 5% slippage min
                    ]);
                    step_data
                }
                PoolType::SushiSwap => {
                    // Type 2 = V2 Sushi
                    encode(&[
                        Token::Uint(U256::from(2)),
                        Token::Address(step.pool),
                        Token::Address(step.token_in),
                        Token::Address(step.token_out),
                        Token::Uint(step.amount_out * 95 / 100),
                    ])
                }
                PoolType::Camelot => {
                    // Type 3 = Camelot
                    encode(&[
                        Token::Uint(U256::from(3)),
                        Token::Address(step.pool),
                        Token::Address(step.token_in),
                        Token::Address(step.token_out),
                        Token::Uint(step.amount_out * 95 / 100),
                    ])
                }
            };
            encoded_steps.push(dex_type);
        }
        
        // Combine all steps
        let all_steps: Vec<Token> = encoded_steps.iter()
            .map(|s| Token::Bytes(s.clone()))
            .collect();
        
        Ok(encode(&[Token::Array(all_steps)]))
    }
}

#[derive(Debug)]
pub struct SimulationResult {
    pub success: bool,
    pub gas_estimate: U256,
    pub gas_cost: U256,
    pub net_profit: U256,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum ExecutorError {
    Provider(String),
    Simulation(String),
    Encoding(String),
    GasTooHigh(U256),
    Send(String),
    NotProfitable,
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutorError::Provider(e) => write!(f, "Provider error: {}", e),
            ExecutorError::Simulation(e) => write!(f, "Simulation failed: {}", e),
            ExecutorError::Encoding(e) => write!(f, "Encoding error: {}", e),
            ExecutorError::GasTooHigh(price) => write!(f, "Gas too high: {:?}", price),
            ExecutorError::Send(e) => write!(f, "Failed to send: {}", e),
            ExecutorError::NotProfitable => write!(f, "Not profitable after gas"),
        }
    }
}

impl std::error::Error for ExecutorError {}
