// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import {YulUtils} from "../src/libraries/YulUtils.sol";

/// @dev External wrapper so expectRevert works (library calls are inlined)
contract MulDivWrapper {
    function mulDiv(uint256 a, uint256 b, uint256 c) external pure returns (uint256) {
        return YulUtils.mulDiv(a, b, c);
    }
}

contract YulUtilsTest is Test {
    MulDivWrapper wrapper;

    function setUp() public {
        wrapper = new MulDivWrapper();
    }

    /*//////////////////////////////////////////////////////////////
                          mulDiv TESTS
    //////////////////////////////////////////////////////////////*/

    function test_mulDiv_basic() public pure {
        assertEq(YulUtils.mulDiv(6, 7, 3), 14);
    }

    function test_mulDiv_truncates() public pure {
        assertEq(YulUtils.mulDiv(10, 3, 7), 4);
    }

    function test_mulDiv_zeroNumerator() public pure {
        assertEq(YulUtils.mulDiv(0, 100, 3), 0);
        assertEq(YulUtils.mulDiv(100, 0, 3), 0);
    }

    function test_mulDiv_divisionByZeroReverts() public {
        vm.expectRevert();
        wrapper.mulDiv(1, 1, 0);
    }

    function test_mulDiv_512bit_noOverflow() public pure {
        uint256 maxVal = type(uint256).max;
        assertEq(YulUtils.mulDiv(maxVal, maxVal, maxVal), maxVal);
    }

    function test_mulDiv_512bit_largeProduct() public pure {
        uint256 two128 = 1 << 128;
        assertEq(YulUtils.mulDiv(two128, two128, two128), two128);
    }

    function test_mulDiv_512bit_precision() public pure {
        uint256 val = (1 << 128) + 1;
        assertEq(YulUtils.mulDiv(val, val, val), val);
    }

    function test_mulDiv_overflowReverts() public {
        // max * max / 1 => result doesn't fit in uint256
        vm.expectRevert();
        wrapper.mulDiv(type(uint256).max, type(uint256).max, 1);
    }

    function testFuzz_mulDiv_identity(uint256 a, uint256 b) public pure {
        vm.assume(b > 0);
        assertEq(YulUtils.mulDiv(a, b, b), a);
    }

    function testFuzz_mulDiv_commutative(uint256 a, uint256 b, uint256 c) public view {
        vm.assume(c > 0);
        // Use the wrapper so reverts are caught by try/catch
        try wrapper.mulDiv(a, b, c) returns (uint256 r1) {
            uint256 r2 = wrapper.mulDiv(b, a, c);
            assertEq(r1, r2);
        } catch {
            // If a*b/c overflows, b*a/c should also revert
            try wrapper.mulDiv(b, a, c) {
                revert("Expected revert for commutative call");
            } catch {}
        }
    }
}
