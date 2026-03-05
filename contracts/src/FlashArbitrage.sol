// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {IBalancerVault, IFlashLoanRecipient} from "./interfaces/IBalancerVault.sol";
import {IUniswapV3Factory, IUniswapV3Pool} from "./interfaces/IUniswapV3.sol";
import {IUniswapV2Router} from "./interfaces/IUniswapV2.sol";
import {IERC20} from "./interfaces/IERC20.sol";

/// @title FlashArbitrage
/// @author MEV Protocol Team
/// @notice High-performance flash loan arbitrage executor
/// @dev Optimized with inline Yul for gas efficiency
contract FlashArbitrage is IFlashLoanRecipient {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    error Unauthorized();
    error InsufficientProfit();
    error InvalidCallback();
    error InvalidFlashLoanData();
    error InvalidInput();
    error InvalidSwapType();
    error AlreadyExecuting();
    error UntrustedTarget();
    error SwapFailed();
    error TransferFailed();
    error ApproveFailed();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    event ArbitrageExecuted(
        address indexed token,
        uint256 amountIn,
        uint256 profit,
        bytes32 indexed pathHash
    );

    event ProfitWithdrawn(address indexed token, uint256 amount);

    /*//////////////////////////////////////////////////////////////
                               CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @dev Balancer Vault address (same on all chains)
    IBalancerVault public constant BALANCER_VAULT = 
        IBalancerVault(0xBA12222222228d8Ba445958a75a0704d566BF2C8);

    /// @dev Minimum profit threshold in basis points (0.1%)
    uint256 public constant MIN_PROFIT_BPS = 10;

    /// @dev Basis points denominator
    uint256 private constant BPS = 10000;

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Contract owner
    address public immutable owner;

    /// @notice Whitelisted executors
    mapping(address => bool) public executors;

    /// @notice Whitelisted V2 routers.
    mapping(address => bool) public trustedV2Routers;

    /// @notice Trusted Uniswap V3 factory.
    address public trustedV3Factory;

    /// @notice Pause state
    bool public paused;

    /// @notice Nonce for replay protection
    uint256 public nonce;

    /// @dev Execution context to prevent forged callbacks and replayed payloads.
    bool private executionActive;
    address private pendingExecutor;
    address private pendingToken;
    uint256 private pendingAmount;
    bytes32 private pendingSwapHash;

    /// @dev Active V3 pool expected to call callback during a swap.
    address private activeV3Pool;
    address private activeV3TokenIn;

    struct SwapInstruction {
        uint8 swapType;
        address target;
        bytes params;
    }

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor() {
        owner = msg.sender;
        executors[msg.sender] = true;
    }

    /*//////////////////////////////////////////////////////////////
                               MODIFIERS
    //////////////////////////////////////////////////////////////*/

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier onlyExecutor() {
        if (!executors[msg.sender]) revert Unauthorized();
        _;
    }

    modifier notPaused() {
        require(!paused, "Paused");
        _;
    }

    /*//////////////////////////////////////////////////////////////
                            FLASH LOAN LOGIC
    //////////////////////////////////////////////////////////////*/

    /// @notice Execute flash loan arbitrage
    /// @param token Token to borrow
    /// @param amount Amount to borrow
    /// @param swapData Encoded swap path data
    function executeArbitrage(
        address token,
        uint256 amount,
        bytes calldata swapData
    ) external onlyExecutor notPaused {
        if (token == address(0) || amount == 0 || swapData.length == 0) revert InvalidInput();
        if (executionActive) revert AlreadyExecuting();

        executionActive = true;
        pendingExecutor = msg.sender;
        pendingToken = token;
        pendingAmount = amount;
        pendingSwapHash = keccak256(swapData);

        // Prepare flash loan
        address[] memory tokens = new address[](1);
        tokens[0] = token;
        
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount;

        // Encode callback data
        bytes memory userData = abi.encode(msg.sender, swapData);

        // Execute flash loan (callback will be triggered)
        BALANCER_VAULT.flashLoan(
            IFlashLoanRecipient(address(this)),
            tokens,
            amounts,
            userData
        );

        // Clear execution context after callback path is complete.
        executionActive = false;
        pendingExecutor = address(0);
        pendingToken = address(0);
        pendingAmount = 0;
        pendingSwapHash = bytes32(0);
        unchecked {
            ++nonce;
        }
    }

    /// @notice Balancer flash loan callback
    /// @dev Called by Balancer Vault after flash loan is issued
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        // Verify callback is from Balancer
        if (msg.sender != address(BALANCER_VAULT)) revert InvalidCallback();
        if (!executionActive) revert InvalidCallback();
        if (tokens.length != 1 || amounts.length != 1 || feeAmounts.length != 1) revert InvalidFlashLoanData();

        // Decode user data
        (address executor, bytes memory swapData) = abi.decode(userData, (address, bytes));
        
        // Verify executor
        if (!executors[executor]) revert Unauthorized();
        if (executor != pendingExecutor) revert InvalidCallback();

        address token = tokens[0];
        uint256 amount = amounts[0];
        uint256 fee = feeAmounts[0];
        if (token != pendingToken || amount != pendingAmount) revert InvalidCallback();
        if (keccak256(swapData) != pendingSwapHash) revert InvalidCallback();

        // Execute swap sequence
        _executeSwaps(token, amount, swapData);

        // Calculate profit
        uint256 balanceAfter = _balanceOf(token);
        uint256 amountOwed = amount + fee;
        
        if (balanceAfter < amountOwed) revert InsufficientProfit();
        
        uint256 profit = balanceAfter - amountOwed;
        
        // Verify minimum profit
        uint256 minProfit = (amount * MIN_PROFIT_BPS) / BPS;
        if (profit < minProfit) revert InsufficientProfit();

        // Repay flash loan
        _safeTransfer(token, address(BALANCER_VAULT), amountOwed);

        // Emit event
        emit ArbitrageExecuted(token, amount, profit, keccak256(swapData));
    }

    /*//////////////////////////////////////////////////////////////
                             SWAP EXECUTION
    //////////////////////////////////////////////////////////////*/

    /// @notice Execute swap sequence
    /// @param token Starting token
    /// @param amount Starting amount
    /// @param swapData Encoded swap instructions
    function _executeSwaps(
        address token,
        uint256 amount,
        bytes memory swapData
    ) internal {
        SwapInstruction[] memory swaps = abi.decode(swapData, (SwapInstruction[]));
        if (swaps.length == 0) revert InvalidInput();

        uint256 currentAmount = amount;
        address currentToken = token;

        for (uint256 i = 0; i < swaps.length; ++i) {
            SwapInstruction memory step = swaps[i];

            // Execute swap based on type
            if (step.swapType == 1) {
                currentAmount = _swapUniV2(step.target, currentToken, currentAmount, step.params);
            } else if (step.swapType == 2) {
                currentAmount = _swapUniV3(step.target, currentToken, currentAmount, step.params);
            } else {
                revert InvalidSwapType();
            }

            currentToken = _decodeTokenOut(step.swapType, step.params);
        }
    }

    function _decodeTokenOut(uint8 swapType, bytes memory params) internal pure returns (address tokenOut) {
        if (swapType == 1) {
            (tokenOut,) = abi.decode(params, (address, uint256));
        } else if (swapType == 2) {
            (tokenOut,,) = abi.decode(params, (address, uint24, uint160));
        } else {
            revert InvalidSwapType();
        }
    }

    /// @notice Execute Uniswap V2 style swap
    function _swapUniV2(
        address router,
        address tokenIn,
        uint256 amountIn,
        bytes memory params
    ) internal returns (uint256 amountOut) {
        (address tokenOut, uint256 minOut) = abi.decode(params, (address, uint256));
        if (router == address(0) || tokenOut == address(0)) revert InvalidInput();
        if (!trustedV2Routers[router]) revert UntrustedTarget();
        
        // Approve router
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);
        
        // Build path
        address[] memory path = new address[](2);
        path[0] = tokenIn;
        path[1] = tokenOut;
        
        // Execute swap
        uint256[] memory amounts = IUniswapV2Router(router).swapExactTokensForTokens(
            amountIn,
            minOut,
            path,
            address(this),
            block.timestamp
        );
        
        amountOut = amounts[amounts.length - 1];
    }

    /// @notice Execute Uniswap V3 style swap
    function _swapUniV3(
        address pool,
        address tokenIn,
        uint256 amountIn,
        bytes memory params
    ) internal returns (uint256 amountOut) {
        (address tokenOut, uint24 fee, uint160 sqrtPriceLimitX96) = 
            abi.decode(params, (address, uint24, uint160));
        fee; // retained in params for pool consistency validation extensions.
        if (pool == address(0) || tokenOut == address(0)) revert InvalidInput();

        address poolToken0 = IUniswapV3Pool(pool).token0();
        address poolToken1 = IUniswapV3Pool(pool).token1();
        if (
            !(
                (tokenIn == poolToken0 && tokenOut == poolToken1) ||
                (tokenIn == poolToken1 && tokenOut == poolToken0)
            )
        ) {
            revert InvalidCallback();
        }
        address v3Factory = trustedV3Factory;
        if (v3Factory == address(0)) revert UntrustedTarget();
        if (IUniswapV3Factory(v3Factory).getPool(tokenIn, tokenOut, fee) != pool) revert UntrustedTarget();
        
        bool zeroForOne = tokenIn < tokenOut;

        activeV3Pool = pool;
        activeV3TokenIn = tokenIn;
        
        // Execute swap on pool
        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimitX96 == 0 
                ? (zeroForOne ? 4295128740 : 1461446703485210103287273052203988822378723970341)
                : sqrtPriceLimitX96,
            abi.encode(tokenIn, tokenOut)
        );

        activeV3Pool = address(0);
        activeV3TokenIn = address(0);
        
        amountOut = uint256(zeroForOne ? -amount1 : -amount0);
    }

    /*//////////////////////////////////////////////////////////////
                         YUL OPTIMIZED HELPERS
    //////////////////////////////////////////////////////////////*/

    /// @notice Get token balance using inline assembly
    function _balanceOf(address token) internal view returns (uint256 bal) {
        assembly {
            // Store selector for balanceOf(address)
            mstore(0x00, 0x70a0823100000000000000000000000000000000000000000000000000000000)
            mstore(0x04, address())
            
            // Call token
            let success := staticcall(gas(), token, 0x00, 0x24, 0x00, 0x20)
            
            if iszero(success) {
                revert(0, 0)
            }
            
            bal := mload(0x00)
        }
    }

    /// @notice Safe transfer using inline assembly
    function _safeTransfer(address token, address to, uint256 amount) internal {
        assembly {
            // Store selector for transfer(address,uint256)
            mstore(0x00, 0xa9059cbb00000000000000000000000000000000000000000000000000000000)
            mstore(0x04, to)
            mstore(0x24, amount)
            
            let success := call(gas(), token, 0, 0x00, 0x44, 0x00, 0x20)
            
            // Check return value
            if iszero(success) {
                revert(0, 0)
            }
            
            // Some tokens don't return a value
            switch returndatasize()
            case 0 {}
            case 0x20 {
                if iszero(mload(0x00)) {
                    revert(0, 0)
                }
            }
            default {
                revert(0, 0)
            }
        }
    }

    /// @notice Safe approve using inline assembly
    function _safeApprove(address token, address spender, uint256 amount) internal {
        assembly {
            // Store selector for approve(address,uint256)
            mstore(0x00, 0x095ea7b300000000000000000000000000000000000000000000000000000000)
            mstore(0x04, spender)
            mstore(0x24, amount)
            
            let success := call(gas(), token, 0, 0x00, 0x44, 0x00, 0x20)
            
            if iszero(success) {
                revert(0, 0)
            }

            switch returndatasize()
            case 0 {}
            case 0x20 {
                if iszero(mload(0x00)) {
                    revert(0, 0)
                }
            }
            default {
                revert(0, 0)
            }
        }
    }

    /*//////////////////////////////////////////////////////////////
                              ADMIN FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /// @notice Add or remove executor
    function setExecutor(address executor, bool status) external onlyOwner {
        executors[executor] = status;
    }

    /// @notice Set trusted V2 router status.
    function setTrustedV2Router(address router, bool status) external onlyOwner {
        if (router == address(0)) revert InvalidInput();
        trustedV2Routers[router] = status;
    }

    /// @notice Set the trusted Uniswap V3 factory.
    function setTrustedV3Factory(address factory) external onlyOwner {
        if (factory == address(0)) revert InvalidInput();
        trustedV3Factory = factory;
    }

    /// @notice Pause/unpause contract
    function setPaused(bool _paused) external onlyOwner {
        paused = _paused;
    }

    /// @notice Withdraw profits
    function withdraw(address token, uint256 amount) external onlyOwner {
        _safeTransfer(token, owner, amount);
        emit ProfitWithdrawn(token, amount);
    }

    /// @notice Withdraw all ETH
    function withdrawETH() external onlyOwner {
        (bool success,) = owner.call{value: address(this).balance}("");
        require(success, "ETH transfer failed");
    }

    /// @notice Emergency token rescue
    function rescueToken(address token) external onlyOwner {
        uint256 balance = _balanceOf(token);
        _safeTransfer(token, owner, balance);
    }

    /*//////////////////////////////////////////////////////////////
                              UNISWAP V3 CALLBACK
    //////////////////////////////////////////////////////////////*/

    /// @notice Uniswap V3 swap callback
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        if (msg.sender != activeV3Pool) revert InvalidCallback();
        (address tokenIn, address tokenOut) = abi.decode(data, (address, address));
        tokenOut;
        
        // Pay the required input amount
        uint256 amountToPay = amount0Delta > 0 ? uint256(amount0Delta) : uint256(amount1Delta);
        if (tokenIn != activeV3TokenIn) revert InvalidCallback();
        _safeTransfer(tokenIn, msg.sender, amountToPay);
    }

    /*//////////////////////////////////////////////////////////////
                               RECEIVE ETH
    //////////////////////////////////////////////////////////////*/

    receive() external payable {}
}
