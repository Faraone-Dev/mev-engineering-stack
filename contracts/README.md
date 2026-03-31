# Smart Contracts — `contracts/`

**MEV execution layer: Balancer V2 flash loans + multi-DEX atomic routing.**

Built with [Foundry](https://book.getfoundry.sh/), heavily optimized with inline Yul assembly.

---

## Contracts

### FlashArbitrage.sol

Atomic flash loan arbitrage via Balancer V2 (0% fee). Borrows tokens, routes through DEX pairs, and repays in a single transaction — reverts if unprofitable.

| Feature | Implementation |
|---------|---------------|
| **Flash Loan Provider** | Balancer V2 Vault (`0xBA12222222228d8Ba445958a75a0704d566BF2C8`) |
| **Fee** | 0% (vs. Aave's 0.09%) |
| **Routing** | Multi-hop sequential swaps (V2 → V3, V3 → V2, or any mix) |
| **Callback Hardening** | Execution context: `(executor, token, amount, swapHash, nonce)` |
| **Yul Assembly** | `_balanceOf()`, `_safeTransfer()`, `_safeApprove()` — gas-optimized ERC20 |
| **Access Control** | `onlyOwner`, `onlyExecutor`, `notPaused` modifiers |
| **Profit Check** | `MIN_PROFIT_BPS = 10` (0.1%) enforced on-chain |

**Callback Security Model:**

```
FlashArbitrage.executeArbitrage()
  │
  ├── Sets context: executionActive, pendingExecutor, pendingToken, pendingAmount, pendingSwapHash
  │
  ├── Calls BalancerVault.flashLoan()
  │     │
  │     └── Vault calls back: receiveFlashLoan()
  │           │
  │           ├── Validates: msg.sender == BALANCER_VAULT
  │           ├── Validates: executionActive == true
  │           ├── Validates: keccak256(executor, token, amount, nonce) == pendingSwapHash
  │           │
  │           ├── Executes swaps via _executeSwaps()
  │           ├── Checks profit >= MIN_PROFIT_BPS
  │           └── Repays loan + sends profit to owner
  │
  └── Clears context + increments nonce (replay protection)
```

### MultiDexRouter.sol

Aggregated DEX routing — calls pools directly (bypasses routers) for gas efficiency.

| Feature | Implementation |
|---------|---------------|
| **V2 Direct** | `swapV2Direct()` — calls `IUniswapV2Pair.swap()` with factory validation |
| **V2 Multi-Hop** | `swapV2MultiHop()` — chains pairs sequentially |
| **V3 Direct** | `swapV3Direct()` — calls `IUniswapV3Pool.swap()` with `sqrtPriceLimitX96` |
| **Mixed Paths** | `executeSwapPath()` — packed calldata for arbitrary V2/V3 sequences |
| **V3 Callback** | Validates `msg.sender == activeV3Pool` before payment |
| **Factory Whitelist** | V2/V3 pools verified against trusted factory addresses |

**Packed Calldata Format** for `executeSwapPath()`:

```
[amountIn: 32 bytes][tokenIn: 20 bytes][numSwaps: 1 byte]
  [swapType: 1 byte][pool: 20 bytes][tokenOut: 20 bytes]  ← repeated numSwaps times
```

### YulUtils.sol (Library)

Pure Yul assembly utility library for gas-critical operations. All functions are `internal pure` for automatic inlining by the compiler — zero external call overhead.

| Category | Functions |
|----------|-----------|
| **Math** | `mulDiv()`, `mulMod()`, `safeSub()`, `sqrt()` (Babylonian) |
| **Address** | `isContract()`, `codeHash()`, `addressLt()` |
| **Memory** | `memoryCopy()`, `freeMemoryPointer()`, `setFreeMemoryPointer()` |
| **Hashing** | `hash2()`, `hashAddressUint()` |
| **Calldata** | `loadCalldataUint()`, `loadCalldataAddress()` |
| **Uniswap V2** | `getAmountOut()`, `getAmountIn()` — constant-product formula in pure assembly |

### Interfaces

| Interface | Coverage |
|-----------|----------|
| `IBalancerVault.sol` | `flashLoan()` + `IFlashLoanRecipient` callback |
| `IERC20.sol` | Standard ERC20 + IWETH (deposit/withdraw) |
| `IUniswapV2.sol` | Pair (swap, getReserves), Router, Factory |
| `IUniswapV3.sol` | Pool (swap, slot0, observe), Factory, SwapRouter, QuoterV2 |

---

## Build & Test

```bash
cd contracts
forge build               # Compile all contracts
forge test -vvv           # Run test suite with verbose output
forge test --gas-report   # Gas usage per function
```

## Tests

| Test Suite | Coverage |
|------------|----------|
| **FlashArbitrage.t.sol** | Access control (6), pause mechanism (2), callback validation (3), fuzz testing (2), invariant testing (1) — **14 tests covering all critical security paths** |
| **MultiDexRouter.t.sol** | Malformed input rejection, V3 callback spoofing prevention |

Tests cover critical security paths: callback origin validation, executor authorization, replay protection, and pause controls.

## Deploy

```bash
# Arbitrum Sepolia (testnet) — uses mock Balancer vault
forge script script/DeployArbitrum.s.sol:DeployArbitrumSepolia \
  --rpc-url $ARBITRUM_SEPOLIA_RPC --broadcast -vvvv

# Arbitrum One (mainnet) — uses real Balancer vault
forge script script/DeployArbitrum.s.sol:DeployArbitrumMainnet \
  --rpc-url $ARBITRUM_RPC_URL --broadcast -vvvv --verify
```

---

## ⚠️ Production Deployment Requirements

These contracts are **audited-ready but not yet deployed**. Going live requires:

1. **Professional security audit** — formal verification of callback logic and flash loan repayment paths
2. **Dedicated Flashbots/MEV relay** — bundle submission to block builders (e.g., Flashbots Protect, MEV Blocker, Merkle)
3. **Co-located infrastructure** — sub-millisecond latency to the Arbitrum sequencer
4. **Capital** — ETH for gas + working capital for profitable flash arbitrage
5. **Monitoring** — real-time alerting on failed bundles, gas spikes, and profit degradation

The current stack runs in **simulation mode**: scanning mainnet in real-time, classifying transactions, and detecting opportunities — without submitting bundles or executing trades.
