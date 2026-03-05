// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {IUniswapV2Factory, IUniswapV2Pair} from "./interfaces/IUniswapV2.sol";
import {IUniswapV3Factory, IUniswapV3Pool} from "./interfaces/IUniswapV3.sol";
import {IERC20} from "./interfaces/IERC20.sol";
import {YulUtils} from "./libraries/YulUtils.sol";

/// @title MultiDexRouter
/// @notice Optimized multi-DEX swap router for MEV extraction
/// @dev Uses direct pool calls instead of routers for gas efficiency
contract MultiDexRouter {
    /*//////////////////////////////////////////////////////////////
                                ERRORS
    //////////////////////////////////////////////////////////////*/

    error InvalidPool();
    error InsufficientOutput();
    error Unauthorized();
    error InvalidInput();
    error InvalidSwapType();
    error InvalidCallback();
    error UntrustedFactory();

    /*//////////////////////////////////////////////////////////////
                               CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @dev Minimum sqrt ratio for Uniswap V3
    uint160 internal constant MIN_SQRT_RATIO = 4295128739;
    
    /// @dev Maximum sqrt ratio for Uniswap V3
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342;

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    address public immutable owner;
    address public trustedV2Factory;
    address public trustedV3Factory;

    /// @dev Active V3 callback context to prevent direct callback spoofing.
    address private activeV3Pool;
    address private activeV3TokenIn;
    address private activeV3Payer;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    function setTrustedFactories(address v2Factory, address v3Factory) external onlyOwner {
        if (v2Factory == address(0) || v3Factory == address(0)) revert InvalidInput();
        trustedV2Factory = v2Factory;
        trustedV3Factory = v3Factory;
    }

    /*//////////////////////////////////////////////////////////////
                            UNISWAP V2 SWAPS
    //////////////////////////////////////////////////////////////*/

    /// @notice Direct swap on Uniswap V2 pair
    /// @dev Bypasses router for gas efficiency
    function swapV2Direct(
        address pair,
        address tokenIn,
        uint256 amountIn,
        uint256 amountOutMin,
        address to
    ) external returns (uint256 amountOut) {
        if (pair == address(0) || tokenIn == address(0) || to == address(0) || amountIn == 0) revert InvalidInput();

        // Get reserves
        (uint112 reserve0, uint112 reserve1,) = IUniswapV2Pair(pair).getReserves();
        
        address token0 = IUniswapV2Pair(pair).token0();
        address token1 = IUniswapV2Pair(pair).token1();
        if (tokenIn != token0 && tokenIn != token1) revert InvalidPool();
        address v2Factory = trustedV2Factory;
        if (v2Factory == address(0)) revert UntrustedFactory();
        if (IUniswapV2Factory(v2Factory).getPair(token0, token1) != pair) revert InvalidPool();
        bool zeroForOne = tokenIn == token0;
        
        (uint112 reserveIn, uint112 reserveOut) = zeroForOne 
            ? (reserve0, reserve1) 
            : (reserve1, reserve0);

        // Calculate output
        amountOut = YulUtils.getAmountOut(amountIn, reserveIn, reserveOut);
        
        if (amountOut < amountOutMin) revert InsufficientOutput();

        // Transfer input tokens to pair
        _safeTransferFrom(tokenIn, msg.sender, pair, amountIn);

        // Execute swap
        (uint256 amount0Out, uint256 amount1Out) = zeroForOne 
            ? (uint256(0), amountOut) 
            : (amountOut, uint256(0));
            
        IUniswapV2Pair(pair).swap(amount0Out, amount1Out, to, "");
    }

    /// @notice Multi-hop V2 swap
    function swapV2MultiHop(
        address[] calldata pairs,
        address[] calldata tokens,
        uint256 amountIn,
        uint256 amountOutMin,
        address to
    ) external returns (uint256 amountOut) {
        if (pairs.length == 0 || tokens.length != pairs.length + 1 || amountIn == 0 || to == address(0)) {
            revert InvalidInput();
        }

        uint256 len = pairs.length;
        amountOut = amountIn;
        
        // Transfer initial tokens
        _safeTransferFrom(tokens[0], msg.sender, pairs[0], amountIn);
        
        for (uint256 i = 0; i < len;) {
            address pair = pairs[i];
            address tokenIn = tokens[i];
            address tokenOut = tokens[i + 1];
            address recipient = i == len - 1 ? to : pairs[i + 1];
            
            // Get reserves
            (uint112 reserve0, uint112 reserve1,) = IUniswapV2Pair(pair).getReserves();
            address token0 = IUniswapV2Pair(pair).token0();
            address token1 = IUniswapV2Pair(pair).token1();
            if (!((tokenIn == token0 && tokenOut == token1) || (tokenIn == token1 && tokenOut == token0))) {
                revert InvalidPool();
            }
            address v2Factory = trustedV2Factory;
            if (v2Factory == address(0)) revert UntrustedFactory();
            if (IUniswapV2Factory(v2Factory).getPair(token0, token1) != pair) revert InvalidPool();
            bool zeroForOne = tokenIn == token0;
            
            (uint112 reserveIn, uint112 reserveOut) = zeroForOne 
                ? (reserve0, reserve1) 
                : (reserve1, reserve0);
            
            // Calculate output
            amountOut = YulUtils.getAmountOut(amountOut, reserveIn, reserveOut);
            
            // Execute swap
            (uint256 amount0Out, uint256 amount1Out) = zeroForOne 
                ? (uint256(0), amountOut) 
                : (amountOut, uint256(0));
                
            IUniswapV2Pair(pair).swap(amount0Out, amount1Out, recipient, "");
            
            unchecked { ++i; }
        }
        
        if (amountOut < amountOutMin) revert InsufficientOutput();
    }

    /*//////////////////////////////////////////////////////////////
                            UNISWAP V3 SWAPS
    //////////////////////////////////////////////////////////////*/

    /// @notice Direct swap on Uniswap V3 pool
    function swapV3Direct(
        address pool,
        address tokenIn,
        uint256 amountIn,
        uint256 amountOutMin,
        address to
    ) external returns (uint256 amountOut) {
        if (pool == address(0) || tokenIn == address(0) || to == address(0) || amountIn == 0) revert InvalidInput();

        address token0 = IUniswapV3Pool(pool).token0();
        address token1 = IUniswapV3Pool(pool).token1();
        if (tokenIn != token0 && tokenIn != token1) revert InvalidPool();
        address v3Factory = trustedV3Factory;
        if (v3Factory == address(0)) revert UntrustedFactory();
        uint24 fee = IUniswapV3Pool(pool).fee();
        if (IUniswapV3Factory(v3Factory).getPool(token0, token1, fee) != pool) revert InvalidPool();
        bool zeroForOne = tokenIn == token0;
        
        // Calculate sqrt price limit
        uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
        
        // Encode callback data
        bytes memory data = abi.encode(tokenIn, msg.sender);

        activeV3Pool = pool;
        activeV3TokenIn = tokenIn;
        activeV3Payer = msg.sender;
        
        // Execute swap
        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            to,
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimitX96,
            data
        );

        activeV3Pool = address(0);
        activeV3TokenIn = address(0);
        activeV3Payer = address(0);
        
        amountOut = uint256(zeroForOne ? -amount1 : -amount0);
        
        if (amountOut < amountOutMin) revert InsufficientOutput();
    }

    /// @notice Uniswap V3 callback
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        (address tokenIn, address payer) = abi.decode(data, (address, address));
        if (msg.sender != activeV3Pool) revert InvalidCallback();
        if (tokenIn != activeV3TokenIn || payer != activeV3Payer) revert InvalidCallback();
        
        uint256 amountToPay = amount0Delta > 0 ? uint256(amount0Delta) : uint256(amount1Delta);
        
        // Internal flows use direct transfer; external flows use transferFrom.
        if (payer == address(this)) {
            _safeTransfer(tokenIn, msg.sender, amountToPay);
        } else {
            _safeTransferFrom(tokenIn, payer, msg.sender, amountToPay);
        }
    }

    /*//////////////////////////////////////////////////////////////
                           COMBINED SWAPS
    //////////////////////////////////////////////////////////////*/

    /// @notice Execute arbitrary swap path across V2 and V3
    /// @param swapData Encoded swap instructions
    function executeSwapPath(
        bytes calldata swapData
    ) external returns (uint256 amountOut) {
        if (swapData.length < 53) revert InvalidInput();

        // Decode: [amountIn][tokenIn][numSwaps][[swapType][pool][tokenOut]...]
        uint256 offset = 0;
        
        uint256 amountIn = uint256(bytes32(swapData[offset:offset+32]));
        offset += 32;
        
        address currentToken = address(bytes20(swapData[offset:offset+20]));
        offset += 20;
        
        uint8 numSwaps = uint8(swapData[offset]);
        offset += 1;
        
        uint256 currentAmount = amountIn;
        
        for (uint8 i = 0; i < numSwaps; i++) {
            uint8 swapType = uint8(swapData[offset]);
            offset += 1;
            
            address pool = address(bytes20(swapData[offset:offset+20]));
            offset += 20;
            
            address tokenOut = address(bytes20(swapData[offset:offset+20]));
            offset += 20;
            
            address recipient = i == numSwaps - 1 ? msg.sender : address(this);
            
            if (swapType == 1) {
                // V2 swap
                currentAmount = _executeV2Swap(pool, currentToken, currentAmount, recipient);
            } else if (swapType == 2) {
                // V3 swap
                currentAmount = _executeV3Swap(pool, currentToken, currentAmount, recipient);
            } else {
                revert InvalidSwapType();
            }
            
            currentToken = tokenOut;
        }
        
        amountOut = currentAmount;
    }

    /*//////////////////////////////////////////////////////////////
                           INTERNAL HELPERS
    //////////////////////////////////////////////////////////////*/

    function _executeV2Swap(
        address pair,
        address tokenIn,
        uint256 amountIn,
        address to
    ) internal returns (uint256 amountOut) {
        if (pair == address(0) || tokenIn == address(0) || to == address(0) || amountIn == 0) revert InvalidInput();

        (uint112 reserve0, uint112 reserve1,) = IUniswapV2Pair(pair).getReserves();
        address token0 = IUniswapV2Pair(pair).token0();
        address token1 = IUniswapV2Pair(pair).token1();
        if (tokenIn != token0 && tokenIn != token1) revert InvalidPool();
        address v2Factory = trustedV2Factory;
        if (v2Factory == address(0)) revert UntrustedFactory();
        if (IUniswapV2Factory(v2Factory).getPair(token0, token1) != pair) revert InvalidPool();
        bool zeroForOne = tokenIn == token0;
        
        (uint112 reserveIn, uint112 reserveOut) = zeroForOne 
            ? (reserve0, reserve1) 
            : (reserve1, reserve0);

        amountOut = YulUtils.getAmountOut(amountIn, reserveIn, reserveOut);
        
        _safeTransfer(tokenIn, pair, amountIn);
        
        (uint256 amount0Out, uint256 amount1Out) = zeroForOne 
            ? (uint256(0), amountOut) 
            : (amountOut, uint256(0));
            
        IUniswapV2Pair(pair).swap(amount0Out, amount1Out, to, "");
    }

    function _executeV3Swap(
        address pool,
        address tokenIn,
        uint256 amountIn,
        address to
    ) internal returns (uint256 amountOut) {
        if (pool == address(0) || tokenIn == address(0) || to == address(0) || amountIn == 0) revert InvalidInput();

        address token0 = IUniswapV3Pool(pool).token0();
        address token1 = IUniswapV3Pool(pool).token1();
        if (tokenIn != token0 && tokenIn != token1) revert InvalidPool();
        address v3Factory = trustedV3Factory;
        if (v3Factory == address(0)) revert UntrustedFactory();
        uint24 fee = IUniswapV3Pool(pool).fee();
        if (IUniswapV3Factory(v3Factory).getPool(token0, token1, fee) != pool) revert InvalidPool();
        bool zeroForOne = tokenIn == token0;
        uint160 sqrtPriceLimitX96 = zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1;
        
        bytes memory data = abi.encode(tokenIn, address(this));

        activeV3Pool = pool;
        activeV3TokenIn = tokenIn;
        activeV3Payer = address(this);
        
        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            to,
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimitX96,
            data
        );

        activeV3Pool = address(0);
        activeV3TokenIn = address(0);
        activeV3Payer = address(0);
        
        amountOut = uint256(zeroForOne ? -amount1 : -amount0);
    }

    function _safeTransfer(address token, address to, uint256 amount) internal {
        assembly {
            mstore(0x00, 0xa9059cbb00000000000000000000000000000000000000000000000000000000)
            mstore(0x04, to)
            mstore(0x24, amount)
            
            let success := call(gas(), token, 0, 0x00, 0x44, 0x00, 0x20)
            
            if iszero(success) { revert(0, 0) }

            switch returndatasize()
            case 0 {}
            case 0x20 {
                if iszero(mload(0x00)) { revert(0, 0) }
            }
            default {
                revert(0, 0)
            }
        }
    }

    function _safeTransferFrom(address token, address from, address to, uint256 amount) internal {
        assembly {
            mstore(0x00, 0x23b872dd00000000000000000000000000000000000000000000000000000000)
            mstore(0x04, from)
            mstore(0x24, to)
            mstore(0x44, amount)
            
            let success := call(gas(), token, 0, 0x00, 0x64, 0x00, 0x20)
            
            if iszero(success) { revert(0, 0) }

            switch returndatasize()
            case 0 {}
            case 0x20 {
                if iszero(mload(0x00)) { revert(0, 0) }
            }
            default {
                revert(0, 0)
            }
        }
    }

    receive() external payable {}
}
