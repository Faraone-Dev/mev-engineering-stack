// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/FlashArbitrage.sol";

contract FlashArbitrageTest is Test {
    FlashArbitrage public flashArb;
    
    address public owner = address(0x1);
    address public executor = address(0x2);
    address public attacker = address(0x3);
    address public constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    
    // Mock tokens
    address public constant WETH = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
    address public constant USDC = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;

    function setUp() public {
        vm.startPrank(owner);
        flashArb = new FlashArbitrage();
        flashArb.setExecutor(executor, true);
        vm.stopPrank();
    }

    /*//////////////////////////////////////////////////////////////
                            ACCESS CONTROL
    //////////////////////////////////////////////////////////////*/

    function test_OnlyOwnerCanSetExecutor() public {
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.Unauthorized.selector);
        flashArb.setExecutor(attacker, true);
    }

    function test_OnlyOwnerCanPause() public {
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.Unauthorized.selector);
        flashArb.setPaused(true);
    }

    function test_OnlyOwnerCanWithdraw() public {
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.Unauthorized.selector);
        flashArb.withdraw(WETH, 1 ether);
    }

    function test_OnlyExecutorCanExecuteArbitrage() public {
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.Unauthorized.selector);
        flashArb.executeArbitrage(WETH, 1 ether, "");
    }

    function test_ExecutorCanBeAdded() public {
        vm.prank(owner);
        flashArb.setExecutor(attacker, true);
        assertTrue(flashArb.executors(attacker));
    }

    function test_ExecutorCanBeRemoved() public {
        vm.prank(owner);
        flashArb.setExecutor(executor, false);
        assertFalse(flashArb.executors(executor));
    }

    /*//////////////////////////////////////////////////////////////
                              PAUSE TESTS
    //////////////////////////////////////////////////////////////*/

    function test_CannotExecuteWhenPaused() public {
        vm.prank(owner);
        flashArb.setPaused(true);
        
        vm.prank(executor);
        vm.expectRevert("Paused");
        flashArb.executeArbitrage(WETH, 1 ether, "");
    }

    function test_CanExecuteWhenUnpaused() public {
        vm.startPrank(owner);
        flashArb.setPaused(true);
        flashArb.setPaused(false);
        vm.stopPrank();
        
        assertFalse(flashArb.paused());
    }

    /*//////////////////////////////////////////////////////////////
                           CALLBACK TESTS
    //////////////////////////////////////////////////////////////*/

    function test_InvalidCallbackReverts() public {
        address[] memory tokens = new address[](1);
        tokens[0] = WETH;
        
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1 ether;
        
        uint256[] memory fees = new uint256[](1);
        fees[0] = 0;
        
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.InvalidCallback.selector);
        flashArb.receiveFlashLoan(tokens, amounts, fees, "");
    }

    function test_BalancerCallbackWithoutActiveExecutionReverts() public {
        address[] memory tokens = new address[](1);
        tokens[0] = WETH;

        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1 ether;

        uint256[] memory fees = new uint256[](1);
        fees[0] = 0;

        vm.prank(BALANCER_VAULT);
        vm.expectRevert(FlashArbitrage.InvalidCallback.selector);
        flashArb.receiveFlashLoan(tokens, amounts, fees, abi.encode(executor, bytes("")));
    }

    function test_UniswapV3CallbackSpoofReverts() public {
        vm.prank(attacker);
        vm.expectRevert(FlashArbitrage.InvalidCallback.selector);
        flashArb.uniswapV3SwapCallback(1, 0, abi.encode(WETH, USDC));
    }

    /*//////////////////////////////////////////////////////////////
                           FUZZ TESTS
    //////////////////////////////////////////////////////////////*/

    function testFuzz_SetExecutor(address newExecutor) public {
        vm.assume(newExecutor != address(0));
        
        vm.prank(owner);
        flashArb.setExecutor(newExecutor, true);
        
        assertTrue(flashArb.executors(newExecutor));
    }

    function testFuzz_OnlyOwnerCanWithdraw(address caller) public {
        vm.assume(caller != owner);
        
        vm.prank(caller);
        vm.expectRevert(FlashArbitrage.Unauthorized.selector);
        flashArb.withdraw(WETH, 1 ether);
    }

    /*//////////////////////////////////////////////////////////////
                          INVARIANT TESTS
    //////////////////////////////////////////////////////////////*/

    function invariant_OwnerNeverChanges() public view {
        assertEq(flashArb.owner(), owner);
    }
}
