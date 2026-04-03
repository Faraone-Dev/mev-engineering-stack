# MEV Protocol — Low-Latency Execution Engine

**Low-latency, multi-language execution pipeline for real-time transaction processing on Arbitrum.**

*Designed and implemented as a solo project.*

![Rust](https://img.shields.io/badge/Rust-Core_Engine-orange?style=flat-square&logo=rust)
![Go](https://img.shields.io/badge/Go-Network_Layer-00ADD8?style=flat-square&logo=go)
![C](https://img.shields.io/badge/C-Hot_Path-A8B9CC?style=flat-square&logo=c)
![Solidity](https://img.shields.io/badge/Solidity-Contracts-363636?style=flat-square&logo=solidity)

---

## 🚀 Overview

- ⚡ End-to-end pipeline: **~600 ns per opportunity** (sub-microsecond internal processing, excluding network latency)
- 🧠 Four-language architecture: Go (network I/O), Rust (detection + simulation), C (SIMD hot paths), Solidity (on-chain execution)
- 🔄 Fault-tolerant: graceful degradation across all layers — no panics, no silent failures
- 📊 240+ tests, 7 benchmark groups, real-time Prometheus dashboard
- 🔒 Constructive MEV only — no sandwich attacks, no front-running

**Focus:** high-performance systems, lock-free concurrency, and reliability — not trading strategies.

> Designed for deterministic sub-microsecond execution under concurrent load, with bounded latency and no blocking in the hot path.

---

## 🧠 Why This Project Exists

This project explores how far a single developer can push:

- **Low-latency system design** — sub-microsecond processing pipeline with Criterion-verified benchmarks
- **Lock-free concurrency** — CAS queues, atomic operations, zero-allocation hot paths at 40.7 ns/op
- **Multi-language architecture tradeoffs** — gRPC vs FFI, Go scheduler vs cgo, Yul vs Solidity
- **Two-stage simulation** — AMM math fast filter (~35 ns) → revm 8.0 fork execution (~50–200 µs) for full EVM validation
- **Production-grade fault tolerance** — exponential backoff, graceful degradation, monitor-only fallback

The goal is not profitability, but engineering performance and system reliability.

---

## ⚙️ Key Engineering Challenges

| Challenge | Solution |
|-----------|----------|
| No public mempool on Arbitrum | Block-based transaction reconstruction with 4-byte selector classification |
| Sub-microsecond latency under concurrent load | Lock-free MPSC queue (CAS slot-claim), arena allocator, crossbeam channels |
| False sharing in lock-free data structures | `alignas(64)` cache-line isolation on head/tail pointers |
| Partial system failures cascading | Monitor-only fallback, exponential backoff, pure-Rust FFI fallbacks |
| Precision-safe 256-bit arithmetic | `checked_mul`/`checked_add`, custom `div_u256_by_u128` with edge-case tests |
| Go ↔ Rust communication overhead | gRPC over FFI — avoids cgo pinning goroutines to OS threads |
| Gas optimization on-chain | Targeted inline Yul in hot loops only, Balancer 0% fee flash loans |

---

## Architecture

```
 ┌────────────────────────────────────────────────────────────────────────────┐
 │                          MEV Protocol Pipeline                             │
 │                                                                            │
 │   Mempool ──▶ Classify ──▶ Detect ──▶ Simulate ──▶ Build ──▶ Submit      │
 │   (Go)        (Go)         (Rust)     (Rust/revm)   (Rust)    (Go relay)  │
 └────────────────────────────────────────────────────────────────────────────┘

          ┌─────────────┐        gRPC         ┌──────────────┐
          │  network/   │◄═══════════════════▶│    core/      │
          │  Go 1.21    │   proto/mev.proto   │  Rust 2021   │
          │             │                      │              │
          │ • mempool   │                      │ • detector   │
          │ • pipeline  │                      │ • simulator  │
          │ • relay     │                      │ • builder    │
          │ • metrics   │                      │ • grpc srv   │
          │ • gas oracle│                      │ • ffi bridge │
          └──────┬──────┘                      └──────┬───────┘
                 │                                     │ FFI
                 │ eth_sendBundle                       │
          ┌──────┴──────┐                      ┌──────┴───────┐
          │ Flashbots   │                      │    fast/      │
          │ Relay       │                      │     C         │
          │             │                      │              │
          │ EIP-191     │                      │ • keccak256  │
          │ Multi-Relay │                      │ • RLP encode │
          │ Race/All    │                      │ • SIMD AVX2  │
          └─────────────┘                      │ • lock-free Q│
                                               │ • mem pool   │
          ┌─────────────┐                      └──────────────┘
          │ contracts/  │
          │ Solidity    │
          │             │
          │ FlashArb    │ ◄── Balancer V2 flash loans (0% fee)
          │ MultiDexRtr │ ◄── V2/V3/Sushi/Curve routing
          └─────────────┘
```

### Layer Breakdown

| Layer | Language | Purpose | Entry Point |
|-------|----------|---------|-------------|
| **network/** | Go 1.21 | Mempool monitoring, tx classification, Flashbots relay, Prometheus metrics | `cmd/mev-node/main.go` |
| **core/** | Rust 2021 | MEV detection (arbitrage + backrun + liquidation), AMM simulation (V2 constant-product + V3 concentrated liquidity), bundle construction, gRPC server | `src/main.rs` |
| **fast/** | C (GCC/Clang) | SIMD keccak (`memcpy`-safe absorb), RLP encoding, lock-free MPSC queue (CAS slot-claim), arena allocator (batch + rollback) | `src/keccak.c` |
| **contracts/** | Solidity + Yul | Flash loan arbitrage (Balancer V2, 0% fee), multi-DEX routing (direct pool calls), inline Yul assembly (ERC20 hot path), YulUtils library (15+ pure assembly helpers) | `src/FlashArbitrage.sol` |
| **proto/** | Protocol Buffers | Cross-language service contract (Go ↔ Rust) | `mev.proto` |

---

## Live Dashboard

Real-time monitoring dashboard polling Prometheus metrics every 2 seconds. Single self-contained HTML file — no build tools, no dependencies.

```
┌──────────────┬──────────────────────────────────────────────┬──────────────┐
│  NETWORK     │        TRANSACTION PROCESSING PIPELINE       │  LIVE FEED   │
│              │  Ingest → Classify → Filter → Opp → Relay   │  55 events   │
│  Block #448M │         623     623     30     181    0      │              │
│  RPC 1/1     │                                              │  PERFORMANCE │
│  Propagation │        REVENUE & P&L           SESSION       │  40.7ns      │
│  1000ms      │        Total Extracted: 0.0000 ETH           │  425ns       │
│              │                                              │  4 workers   │
│  TX SOURCE   │        CLASSIFICATION BREAKDOWN              │              │
│  Classified  │   V2: 0   V3: 30   Transfer: 151            │  ERRORS      │
│  623         │                                              │  0  0  0     │
│              │        EIP-1559 GAS ORACLE   250ms           │              │
│  Buffer 6%   │   Base: 0.020  Priority: 0.01  Pred: 0.022  │              │
└──────────────┴──────────────────────────────────────────────┴──────────────┘
```

**Features:**
- 3-column layout: Network stats, pipeline center, live feed + performance
- Transaction Processing Pipeline with animated particle flow
- Classification Breakdown (Swap V2, V3, Liquidation, Flash Loan, Transfer)
- EIP-1559 Gas Oracle with base fee prediction gauges
- Revenue & P&L tracking (session-scoped)
- Multi-RPC health indicator (healthy / total endpoints)
- Live event feed with color-coded OPP / BLOCK badges

```bash
# 1. Start the Rust engine (serves Prometheus on :9091)
cargo run --release --bin mev-engine --manifest-path core/Cargo.toml

# Optional: enable Stage 2 revm fork validation in gRPC pipeline
MEV_ENABLE_FORK_SIM=1 cargo run --release --bin mev-engine --manifest-path core/Cargo.toml

# 2. Open dashboard in browser
open dashboard/index.html
```

---

## Performance Characteristics

All benchmarks on Intel i5-8250U @ 1.60GHz, [Criterion 0.5](https://bheisler.github.io/criterion.rs/book/). Production targets co-located bare-metal.

### Pipeline Latency Profile

| Stage | p50 | p99 | p999 | Notes |
|-------|-----|-----|------|-------|
| Classification (Go) | 40 ns | ~65 ns | ~110 ns | zero alloc, branch-predictable selector dispatch |
| Detection (Rust) | 120 ns | ~210 ns | ~400 ns | lock-free queue input, cached pool state |
| Simulation (Rust) | 220 ns | ~350 ns | ~700 ns | constant-product fast-path, checked arithmetic |
| Bundle construction | 53 ns | ~80 ns | ~150 ns | ABI encode, packed 72-byte path |
| **Full pipeline** | **608 ns** | **~1.1 µs** | **~2.3 µs** | **excludes network** |

> p99/p999 estimated from Criterion distribution tails. Full pipeline processes a transaction **1500× faster** than Arbitrum's 250ms block time.

Tail latency dominated by:
- Cross-thread handoff (crossbeam bounded channel)
- Cache misses on cold pool lookups (DashMap, 10k entries)
- Keccak-256 hashing (552 ns, address verification)

### Per-Operation Benchmarks

| Operation | Latency | Notes |
|-----------|---------|-------|
| Keccak-256 (32 bytes) | **552 ns** | `tiny-keccak` — address hashing |
| Constant-product swap (1 ETH) | **35 ns** | x·y = k with 30 bps fee |
| Two-hop arbitrage (buy DEX A → sell DEX B) | **69 ns** | Cross-DEX price discrepancy |
| Pool lookup (10k DashMap) | **55 ns** | Concurrent read, pre-populated |
| ABI encode swap path (3-hop) | **53 ns** | 72-byte packed path |
| **Full pipeline (detect → simulate → build)** | **608 ns** | End-to-end per opportunity |
| U256 mul + div | **83 ns** | `alloy` 256-bit arithmetic |
| Crossbeam channel send+recv | **23 ns** | Bounded 4096, single item |
| Tx classification (Go) | **40.7 ns/op** | 0 B / 0 alloc — ~24.5M tx/sec |
| EIP-1559 base fee calc (Go) | **425 ns/op** | 152 B / 6 alloc — ~2.3M/sec |

---

## Latency Budget

```
┌─────────────────────────────────┬──────────────┬─────────────┐
│  Component                      │  Latency     │  % of total │
├─────────────────────────────────┼──────────────┼─────────────┤
│  Arbitrum RPC (network)         │  1–5 ms      │  ~99.9%     │
│  gRPC serialization (Go↔Rust)   │  5–20 µs     │  ~0.09%     │
│  Internal pipeline              │  ~0.6 µs     │  ~0.01%     │
└─────────────────────────────────┴──────────────┴─────────────┘
```

> Network dominates by ~1000×. The system is optimized for **deterministic internal latency**, not network speed. Every microsecond saved in compute is meaningless if RPC adds 3ms of jitter — but deterministic execution means consistent behavior under load, which matters for queue ordering and opportunity capture.

---

## Concurrency & Contention Model

**Hot path (zero contention):**
- MPSC lock-free queue: CAS slot-claim → write → release fence → atomic count increment
- Single-writer per slot → no write contention, no ABA problem
- `alignas(64)` head/tail → eliminates false sharing across cache lines
- Bounded queues (4096) → prevents unbounded latency growth

**Backpressure strategy:**
- Queue saturated → drop + degrade to monitor-only mode
- No blocking in hot path — ever
- Crossbeam bounded channels between detector→simulator→builder stages

**Thread model:**
- Go: goroutine pool for classification (no cgo, scheduler-friendly)
- Rust: Tokio multi-threaded runtime + crossbeam worker pool for parallel detection
- C: called via FFI from Rust — single-threaded per invocation, no locking

---

## Memory & Allocation Strategy

| Component | Strategy | GC Impact |
|-----------|----------|-----------|
| Go classifier | Zero allocations (40.7 ns/op, 0 B/op) | None in hot path |
| C queue + pool | Arena allocator with batch rollback, preallocated slots | N/A |
| C tx parser | Stack-allocated decode buffers, length-validated | N/A |
| Rust detector | `DashMap` pre-populated pool cache, stack-local `PendingTx` | N/A |
| Rust simulator | `checked_mul`/`checked_add` on stack, no heap per simulation | N/A |
| Go metrics/logging | Standard allocations — confined to non-hot paths | GC here only |

> Go GC is confined to metrics, logging, and configuration. Classification and selector dispatch are allocation-free. C arena allocator supports atomic rollback on partial batch failure.

---

## Behavior Under Load

Scenario: burst of 10k+ transactions per block.

| Failure Mode | Response | Guarantee |
|-------------|----------|-----------|
| Queue saturation | Bounded MPSC drops excess, switches to monitor-only | No unbounded memory growth |
| Detection lag | Degrades to classify-only (skips Rust core) | Pipeline never blocks |
| RPC lag / timeout | Multi-endpoint failover, latency-based routing | No single point of failure |
| gRPC overload | Token-bucket rate limiter (1000 RPS, packed AtomicU64) | Predictable throughput cap |
| WebSocket disconnect | Exponential backoff (1s → 2s → 4s … 30s cap) | No RPC hammering |
| C library missing | Pure-Rust fallback (keccak, RLP, price impact) | Compiles and runs without C |

**Guarantees under all load conditions:**
- No unbounded memory growth
- No blocking in hot path
- No cascading failure across layers
- Bounded queue depth = bounded worst-case latency

---

## Non-Goals / Not Optimized

| Not Implemented | Why |
|----------------|-----|
| Kernel bypass (DPDK / io_uring) | Network latency (~ms) dominates; kernel bypass saves ~µs on a ms-bound path |
| NUMA pinning | Single-node assumption; would matter on multi-socket servers |
| FPGA / hardware acceleration | Compute path is already sub-µs; hardware offload ROI is negative at this scale |
| Userspace networking | Same reasoning as DPDK — bottleneck is RPC, not NIC |
| Custom memory allocator (jemalloc) | Arena allocator in C hot path is sufficient; Rust default allocator performs well |

> Focus is strictly on: **deterministic user-space execution** and **minimal jitter in the compute path**. Network-bound systems benefit from reliability, not raw NIC speed.

---

## Go Network Layer — `network/`

Production-grade mempool monitoring used as the entry point for the MEV pipeline. See [network/README.md](network/README.md) for full documentation.

| Package | Role |
|---------|------|
| `internal/mempool` | WebSocket pending-tx subscription (`gethclient`), selector filtering, **gRPC forwarding to Rust core** with 100ms timeout and graceful fallback |
| `internal/pipeline` | Multi-worker classifier — UniswapV2 (6 selectors), V3 (4), ERC20, Aave, flash loans. **Zero-allocation hot path at 40.7 ns/op** |
| `internal/block` | New-head subscription with reorg detection, polling fallback. **`BlockTxChan()` for block-based tx feed on L2 without public mempool** |
| `internal/gas` | EIP-1559 base fee oracle with multi-block prediction |
| `internal/relay` | Flashbots `eth_sendBundle` + multi-relay manager (Race / Primary / All). **EIP-191 bundle signing, `eth_callBundle` dry-run, `flashbots_getBundleStats`** |
| `internal/rpc` | Connection pool, health checks, latency-based routing, automatic reconnection |
| `internal/metrics` | **20+ Prometheus metrics**: RPC latency histograms, mempool buffer usage, pipeline classification breakdown, relay submission success/failure, gas oracle tracking, node health |
| `cmd/mev-node` | Main binary — pipeline orchestration |
| `cmd/testnet-verify` | Testnet signing verification (EIP-1559 tx + EIP-191 bundle proof) |

```bash
cd network
go build ./...            # compile all binaries
go test ./... -v          # 23 tests across 4 packages
go test -bench . ./...    # selector + gas oracle benchmarks
```

---

## Rust Core Engine — `core/`

High-performance detection, simulation, and bundle construction. See [core/README.md](core/README.md) for full documentation.

- **revm 8.0** — two-stage simulation: AMM math fast filter (35 ns) + fork-mode EVM execution for full state validation
- **Tokio 1.35** — async multi-threaded runtime
- **crossbeam** — lock-free channels for detector→simulator pipeline
- **alloy + ethers** — type-safe Ethereum primitives and ABI encoding
- **Prometheus** — `metrics-exporter-prometheus` with custom TCP server (:9091, CORS-enabled) for real-time dashboard
- **Block-based TX classifier** — classifies every transaction by 4-byte selector (V2/V3 swaps, transfers) since Arbitrum has no public mempool
- **tonic + prost** — gRPC server exposing detection pipeline to Go

```bash
cd core
cargo build --release     # opt-level=3, lto=fat, codegen-units=1
cargo test                # 192 tests (158 unit + 11 integration + 23 proptest)
cargo bench               # 7 Criterion benchmark groups
```

### Detection Pipeline

```
PendingTx → parse_swap() → ArbitrageDetector  ──┐
            (8 selectors,   BackrunDetector   ────┼─▶ Stage 1: AMM Math ──▶ Stage 2: revm Fork ──▶ BundleBuilder ──▶ Bundle
             checked math)  LiquidationDetector─┘    (35 ns filter)        (full EVM validate)     (ABI encode)
```

- **ArbitrageDetector**: Cross-DEX price discrepancy with cached pool state, checked arithmetic
- **BackrunDetector**: Price recovery capture after large swaps with impact threshold
- **LiquidationDetector**: Aave V3, Compound V3, Morpho position tracking with close factor limits
- **MultiThreaded**: Parallel worker pool via crossbeam channels
- **Simulator (two-stage)**: Stage 1 — V2 constant-product (35 ns) + V3 concentrated liquidity via `sqrtPriceX96`, auto-routing by pool type. Stage 2 — `EvmForkSimulator` runs survivors through revm 8.0 fork execution with `CacheDB`, full state diff extraction, and revert decoding

### gRPC Bridge (Go ↔ Rust)

| Component | Location | Protocol |
|-----------|----------|----------|
| Service definition | `proto/mev.proto` | `MevEngine` — 3 RPCs |
| Rust server | `core/src/grpc/server.rs` | tonic 0.11 |
| Go client | `network/internal/strategy/client.go` | google.golang.org/grpc 1.60 |

RPCs: `DetectOpportunity` (unary detect+simulate+build), `StreamOpportunities` (server-streaming via `tokio::broadcast` with profit threshold filter), `GetStatus` (health + uptime + counters)

Target: **< 10ms** round-trip for detect + simulate + bundle on co-located infra.

---

## Smart Contracts — `contracts/`

Solidity + targeted inline Yul assembly. Foundry-based build, **26 tests** (access control, callback hardening, fuzz, invariant, YulUtils 512-bit math).

**Architecture:** Balancer V2 flash loan (0% fee vs Aave's 0.09%) → multi-hop atomic swaps → profit check → repay. Single-tx execution, reverts if unprofitable.

### Contract Overview

| Contract | Purpose | Gas Optimization |
|----------|---------|-----------------|
| **FlashArbitrage.sol** | Balancer V2 flash loan → multi-DEX routing, callback hardening | Inline Yul: `_balanceOf()`, `_safeTransfer()`, `_safeApprove()` skip ABI encoder/decoder |
| **MultiDexRouter.sol** | V2/V3/Sushi direct pool calls (bypasses routers), packed calldata paths | Uses `YulUtils.sol` for all AMM math + calldata parsing |
| **YulUtils.sol** | Pure Yul assembly library — 15+ functions, `internal pure` for compiler inlining | Zero external call overhead: `mulDiv()`, `sqrt()`, `getAmountOut()`, `hash2()`, `loadCalldataAddress()` |

### Why Yul

In MEV, gas saved = profit captured. Yul is used **only** in the hot loop (ERC20 ops called per swap, AMM math per hop), not in business logic:

```
executeArbitrage()              ← Solidity (readable, auditable)
  └─ receiveFlashLoan()         ← Solidity (5-field callback validation)
       └─ _executeSwaps()       ← Solidity (routing logic)
            ├─ _swapUniV2()     ← Solidity + Yul (_safeApprove, _safeTransfer)
            ├─ _swapUniV3()     ← Solidity + Yul (_safeTransfer in callback)
            └─ getAmountOut()   ← YulUtils (pure assembly, constant-product)
```

### Callback Security Model

5-field execution context prevents forged callbacks — even if attacker controls a malicious token:

```
executeArbitrage() sets: executionActive, pendingExecutor, pendingToken, pendingAmount, pendingSwapHash
  → Vault calls receiveFlashLoan()
    → Validates: msg.sender == BALANCER_VAULT
    → Validates: executionActive == true
    → Validates: keccak256(executor, token, amount, nonce) == pendingSwapHash
    → Executes swaps → checks profit ≥ MIN_PROFIT_BPS (0.1%) → repays loan
  → Clears context + increments nonce (replay protection)
```

### Packed Calldata (MultiDexRouter)

`executeSwapPath()` uses custom-packed encoding instead of ABI — parsed with Yul `loadCalldataAddress()`:

```
[amountIn: 32B][tokenIn: 20B][numSwaps: 1B][swapType: 1B][pool: 20B][tokenOut: 20B] × N
```

### Interfaces

| Interface | Coverage |
|-----------|----------|
| `IBalancerVault.sol` | `flashLoan()` + `IFlashLoanRecipient` callback |
| `IERC20.sol` | Standard ERC20 + IWETH (deposit/withdraw) |
| `IUniswapV2.sol` | Pair (swap, getReserves), Router, Factory |
| `IUniswapV3.sol` | Pool (swap, slot0, observe), Factory, SwapRouter, QuoterV2 |

### Deploy & Test

```bash
cd contracts
forge build                  # Compile all
forge test -vvv              # 26 tests — access control (6), callback (3), fuzz (1), invariant (1), pause, edge cases + YulUtils (10)
forge test --gas-report      # Per-function gas usage
forge script script/DeployArbitrum.s.sol:DeployArbitrumSepolia --rpc-url $RPC --broadcast   # Testnet deploy
```

### Folder Structure

```
contracts/
├── src/
│   ├── FlashArbitrage.sol      # Flash loan + multi-DEX execution
│   ├── MultiDexRouter.sol      # Direct pool routing + packed calldata
│   ├── interfaces/             # IBalancerVault, IERC20, IUniswapV2, IUniswapV3
│   └── libraries/
│       └── YulUtils.sol        # Pure Yul assembly: math, memory, hashing, calldata, AMM
├── test/
│   ├── FlashArbitrage.t.sol    # Foundry test suite (14 tests)
│   ├── MultiDexRouter.t.sol    # Router tests
│   └── YulUtils.t.sol          # 512-bit mulDiv precision + fuzz tests (10 tests)
├── script/
│   ├── Deploy.s.sol            # Generic deploy script
│   └── DeployArbitrum.s.sol    # Arbitrum Sepolia + Mainnet deploy
└── foundry.toml                # Optimizer: 10000 runs, via-ir enabled
```

---

## C Hot Path — `fast/`

SIMD-accelerated primitives linked into Rust via FFI (`core/src/ffi/`).

| File | Function | Optimization | Safety |
|------|----------|-------------|--------|
| `keccak.c` | Keccak-256 (24-round permutation, Ethereum `0x01` padding) | Batch hashing | `memcpy` absorb — no alignment UB |
| `rlp.c` | RLP encoding (tx serialization) | Zero-copy output | Bounds-checked length prefixes |
| `simd_utils.c` | Byte comparison, address matching, batch price impact (`__uint128_t`) | AVX2 + SSE4.2, `_mm_prefetch` binary search, non-temporal stores | Mixed-case hex decode (A-F + a-f) |
| `lockfree_queue.c` | MPSC queue (detector→simulator) | CAS-only, no mutex | `alignas(64)` head/tail — no false sharing, CAS slot-claim before write |
| `memory_pool.c` | Arena allocator (3 specialized pools) | Zero-alloc per tx, batch alloc | Atomic rollback on partial batch failure |
| `parser.c` | Binary calldata parsing (V2/V3 ABI) | Unrolled loops | Length validation before decode |

Compile flags: `-O3 -march=native -mavx2 -msse4.2 -flto -falign-functions=64`

```bash
cd fast
make            # → lib/libmev_fast.a + lib/libmev_fast.so
make test       # correctness + SIMD validation
make bench      # hot-path benchmarks
```

---

## Quick Start

### Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| Rust | 1.75+ | Core engine |
| Go | 1.21+ | Network layer |
| Foundry | Latest | Contract compilation + testing |
| GCC / Clang | 11+ | C hot path (AVX2 support) |

### Build & Run

```bash
# 1. Clone
git clone https://github.com/Faraone-Dev/mev-engineering-stack.git
cd mev-engineering-stack

# 2. Configure
cp .env.example .env
# Edit .env with your RPC endpoints and signing key

# 3. Build
make build        # all layers (or build individually below)

# 4. Test
make test         # all layers

# 5. Run
cd network && go run ./cmd/mev-node/

# 6. Benchmark
cd core && cargo bench
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `MEV_RPC_ENDPOINTS` | Comma-separated WebSocket RPC URLs | — |
| `ARBITRUM_RPC_URL` | Arbitrum HTTP endpoint | — |
| `ARBITRUM_WS_URL` | Arbitrum WebSocket endpoint | — |
| `EXECUTE_MODE` | Bundle execution mode (`simulate` or `live`) | `simulate` |
| `PRIVATE_KEY` | EOA key used to sign EIP-1559 executor transactions | — |
| `FLASHBOTS_SIGNING_KEY` | ECDSA key for bundle signing (EIP-191) | — |
| `MEV_USE_FFI` | Enable C fast-path wrappers (`1`/`true`) when available | `false` |
| `MEV_PIPELINE_WORKERS` | Parallel classification workers | `4` |
| `MEV_METRICS_ADDR` | Prometheus metrics bind address | `:9090` |

See [.env.example](.env.example) for the full list. See [CONFIG.md](CONFIG.md) for detailed field descriptions of all configuration files.

---

## Testnet Verification

The `testnet-verify` tool proves the entire signing + bundle pipeline end-to-end against Arbitrum Sepolia without spending gas:

```bash
cd network
go run ./cmd/testnet-verify/

# Output:
# ✓ Signing Key    : 0xa2F3...
# ✓ Chain          : Arbitrum Sepolia (421614)
# ✓ EIP-1559 Tx    : Signed (111 bytes)
# ✓ EIP-191 Sign   : Verified (Flashbots format)
# ✓ Bundle         : Target block N+1
# ○ Submission     : Dry-run (use --submit)
```

Flags: `--key` (reuse signing key), `--rpc` (custom RPC), `--submit` (live submission).

---

## Test Coverage

| Layer | Tests | Framework | What's Tested |
|-------|-------|-----------|---------------|
| **Rust core** | **192 tests** (158 unit + 11 integration + 23 proptest) | `cargo test` + proptest + Criterion | See breakdown below |
| **Go network** | 23 tests, 2 benchmarks | `go test` | Config parsing, EIP-1559 oracle, tx classification (V2/V3 selectors), multi-relay strategies |
| **Rust bench** | 7 groups | Criterion 0.5 | Full pipeline, keccak, AMM, pool lookup, ABI, U256, crossbeam |
| **Solidity** | **26 tests** | `forge test` | Flash arbitrage execution, multi-DEX routing, callback validation, YulUtils 512-bit mulDiv |
| **C hot path** | `make test` | Custom runner | Keccak correctness, RLP encoding, SIMD validation |

### Rust Core — 192 Tests Breakdown

| Module | Tests | Coverage |
|--------|-------|----------|
| `detector/arbitrage` | 26 | V2/V3/V3-output calldata parsing, ABI word decoders, `decode_addr`, `dex_from_fee`, `match_pool_to_swap`, profit calc (profitable/gas-exceeds/no-arb), truncated calldata |
| `detector/backrun` | 3 | Swap selectors, price impact, small-swap filter |
| `detector/liquidation` | 4 | Liquidation detection, healthy skip, close factor, stale pruning |
| `detector/multi_threaded` | 1 | Parallel swap simulation |
| `simulator` | 19 | Constant-product math (happy/zero/overflow/fee=100%), pool cache (load/update/get/reserves fallback), simulate (arb/backrun/liquidation/bundle), success rate |
| `simulator/evm` | 9 | ForkDB insert/query + storage slots, revm ETH transfer, revert/panic decoding, calldata encode roundtrip, profit extraction, metrics counter, BlockContext update |
| `builder` | 16 | ABI encoding, all 3 bundle types, swap path (2-hop/empty/missing pool), no-contract error, count |
| `config` | 9 | Serde roundtrip, save/reload, `from_env` fallback, chain defaults, strategy, performance |
| `ffi/hot_path` | 24 | Keccak-256 known vectors, function selectors (`transfer`/`approve`/V2 swap), `address_eq`, RLP encoding, `OpportunityQueue` FIFO, `TxBuffer` cap, `SwapInfoFFI` |
| `ffi` | 3 | Keccak fallback, RLP single byte, RLP short string |
| `grpc/server` | 3 | `bytes_to_u128` edge cases |
| `mempool/ultra_ws` | 11 | Tx hash extraction, swap classification (V2/V3/Universal Router), non-swap rejection |
| `arbitrum/pools` | 12 | AMM `get_amount_out` (basic/reverse/zero/high-fee), `get_price`, token list validation |
| `types` | 8 | `estimate_gas` for all DexType × OpportunityType combinations |
| `proptest` | 23 | Constant-product invariants (7), ABI roundtrip (3), gas bounds (5), keccak (3), data structures (2), swap selectors (1), overflow safety (2) |
| `integration` | 11 | Full pipeline end-to-end, engine lifecycle, all bundle types, simulator count, gRPC E2E |

### CI/CD

GitHub Actions pipeline with **3 parallel jobs** — each layer builds and tests independently:

| Job | Steps | Toolchain |
|-----|-------|-----------|
| **Rust Core** | `cargo fmt --check` → `cargo clippy -D warnings` → `cargo test` → `cargo build --release` | `dtolnay/rust-toolchain@stable` + `rust-cache` |
| **Go Network** | `go vet` → `go test ./...` → `go build ./cmd/mev-node` | Go 1.21 |
| **Solidity Contracts** | `forge build` → `forge test -vv` | Foundry nightly |

### Bugs Found & Fixed

| Bug | Severity | Module | Fix |
|-----|----------|--------|-----|
| `is_likely_swap()` — OR condition classified all non-zero-first-byte txs as swaps | **Critical** | `mempool/ultra_ws` | Removed `\|\| (selector[0] != 0x00)` |
| `constant_product_swap()` — unchecked u128 arithmetic silently overflowed on whale trades | **High** | `simulator` | `checked_mul`/`checked_add`, returns 0 on overflow |
| Proptest tested local copy of AMM function, not production code | **High** | `tests/proptest` | Now imports `mev_core::simulator::constant_product_swap` directly |
| `div_u256_by_u128` — Knuth division produced wrong quotient on large dividends | **High** | `simulator` | Replaced with standalone loop-based algorithm, 3 edge-case tests |
| `pool_put()` — race condition: count incremented before data written | **High** | `fast/memory_pool.c` | CAS slot-claim → write → release fence → atomic count increment |
| WebSocket reconnection — no backoff on disconnect, hammered RPC on failure | **Medium** | `mempool/ultra_ws` | Exponential backoff (1s → 2s → 4s … 30s cap) |
| gRPC server — no rate limiting, vulnerable to request flooding | **Medium** | `grpc/server` | Token-bucket rate limiter (1000 RPS, packed AtomicU64) |
| FlashArbitrage `require(string)` — wastes gas on revert strings | **Low** | `contracts` | Custom error `ContractPaused()` replaces require string |
| Dashboard unauthenticated — no mention in security docs | **Info** | `SECURITY.md` | Added dashboard authentication section |

---

## Design Decisions

| Decision | Rationale | Tradeoff |
|----------|-----------|----------|
| **4 languages** | Go for concurrent network I/O, Rust for safe high-perf compute, C for SIMD hot paths, Solidity for on-chain | Operational complexity vs optimal tool per domain |
| **gRPC over FFI for Go↔Rust** | Avoids cgo thread pinning → preserves Go scheduler fairness. Isolates failure domains (process boundary) | Adds ~5–20 µs overhead, acceptable vs ms-level network latency |
| **revm over forked geth** | Pure Rust, no cgo dependency, deterministic gas. Two-stage: AMM math filter (35 ns) screens candidates, revm fork execution validates survivors | revm fork adds ~50–200 µs per call, justified only for Stage 1 survivors |
| **Balancer flash loans** | 0% fee vs Aave's 0.09%. When margins are basis points, eliminating the fee is critical | Balancer pool TVL limits flash loan size |
| **Constant-product fast filter** | x·y=k at 35 ns screens candidates before expensive simulation. Only survivors hit EVM | Misses V3 concentrated liquidity edge cases |
| **Arbitrum-first** | 10–100× cheaper gas, 250ms blocks, less MEV competition | No public mempool → requires block-based reconstruction |
| **Monitor-only fallback** | Go node degrades gracefully when Rust core is offline — logs, doesn't crash | Misses opportunities during degraded mode |
| **Bounded queues everywhere** | Prevents unbounded memory growth, gives worst-case latency guarantees | Drops excess transactions under extreme load |

---

## Fault Tolerance

Every layer degrades gracefully. No panics, no silent failures.

| Failure | Response | Recovery |
|---------|----------|----------|
| **RPC endpoint down** | Health check detects within 30s, routes to lowest-latency healthy client | Auto-failover to remaining endpoints |
| **All RPCs unhealthy** | Falls back to first available client | Continues operating in degraded mode |
| **WebSocket disconnects** | Logs error, sleeps 1s, reconnects automatically | Mempool monitor self-heals |
| **WS subscription fails** | Block watcher falls back to HTTP polling (250ms interval) | Transparent — no data loss |
| **Rust gRPC core offline** | Go node enters monitor-only mode — classifies without detection | Resumes full pipeline when core reconnects |
| **C library missing** | Rust FFI auto-switches to pure-Rust fallbacks (keccak, RLP, price impact) | Compiles and runs without C toolchain |
| **Pool cache cold** | Arbitrage detector uses `estimate_cross_dex_prices()` with typical reserves | Bootstraps until live data populates cache |
| **Block fetch overload** | Semaphore limits to 4 concurrent goroutines, 10s timeout | Prevents RPC saturation on fast chains |
| **Primary relay fails** | Multi-relay manager tries fallback relays sequentially | Race / Primary+Fallback / All strategies |
| **Config missing** | `envString(key, fallback)` pattern on every variable | Sensible defaults — never crashes on missing env |

---

## MEV Ethics

This stack extracts **constructive MEV only**:

| Type | Status | Impact |
|------|--------|--------|
| **Arbitrage** | ✅ Supported | Aligns prices across DEXs — improves market efficiency |
| **Backrun** | ✅ Supported | Captures residual slippage after large swaps — no victim |
| **Liquidation** | ✅ Supported | Closes undercollateralized positions — maintains protocol solvency |
| **Sandwich attack** | ❌ Not implemented | Front-run + back-run a victim to steal slippage — predatory |
| **Front-running** | ❌ Not implemented | Copy and front-run pending transactions — predatory |
| **JIT liquidity** | ❌ Not implemented | Temporary liquidity manipulation — market distortion |

> All detected opportunities are non-predatory. No user transactions are harmed, front-run, or sandwiched.

---

## Project Structure

```
mev-engineering-stack/
├── core/                   # Rust — detection, simulation, bundle construction
│   ├── src/
│   │   ├── detector/       # ArbitrageDetector, BackrunDetector, LiquidationDetector
│   │   ├── simulator/      # EvmSimulator — V2 constant-product (35 ns) + V3 concentrated liquidity (sqrtPriceX96), auto-routing
│   │   ├── builder/        # BundleBuilder (ABI encoding, gas pricing)
│   │   ├── grpc/           # tonic server — DetectOpportunity, StreamOpportunities (broadcast channel + profit filter), GetStatus
│   │   ├── arbitrum/       # Arbitrum L2 engine: 3-DEX pool discovery (V3/Sushi/Camelot), triangular arb detection, binary search optimal amount, Balancer V2 flash executor
│   │   ├── ffi/            # C FFI bindings (keccak, RLP, SIMD, queue)
│   │   └── mempool/        # WebSocket data handling
│   └── benches/            # Criterion benchmarks (7 groups)
├── network/                # Go — mempool monitor, pipeline, relay
│   ├── cmd/mev-node/       # Main binary
│   ├── cmd/testnet-verify/ # Testnet signing verification tool
│   ├── internal/           # block, gas, mempool, pipeline, relay, rpc, metrics
│   └── pkg/                # config, types (public packages)
├── contracts/              # Solidity + Yul — flash arbitrage, multi-DEX routing
│   ├── src/
│   │   ├── FlashArbitrage.sol    # Balancer V2 flash loan (0% fee), 5-field callback hardening, inline Yul ERC20
│   │   ├── MultiDexRouter.sol    # Direct pool calls (V2/V3/Sushi), packed calldata encoding
│   │   ├── libraries/
│   │   │   └── YulUtils.sol      # Pure Yul assembly: mulDiv, sqrt, getAmountOut, hash2, calldata parsing (15+ fns)
│   │   └── interfaces/           # IBalancerVault, IERC20/IWETH, IUniswapV2, IUniswapV3
│   ├── test/                     # Foundry: 26 tests (access control, callback, fuzz, invariant, YulUtils)
│   └── script/                   # Deploy: Arbitrum Sepolia + Mainnet
├── fast/                   # C — SIMD keccak, RLP, lock-free queue, memory pool
│   ├── src/                # Implementation (6 files)
│   ├── include/            # Headers
│   └── test/               # Test suite
├── proto/                  # gRPC service definition
├── dashboard/              # Real-time monitoring (HTML/JS)
├── config/                 # Chain + DEX + token configs (JSON)
├── docker/                 # Dockerfile, docker-compose, prometheus.yml
├── scripts/                # Build and deploy automation
├── Makefile                # Top-level build orchestration
└── .env.example            # Environment template (no secrets)
```

---

## License

Proprietary. See [LICENSE](LICENSE) for details.

---

## ⚠️ Execution Modes & Risk Posture

This stack supports both read-only simulation and signed relay submission:

- `simulate` (default): detect, simulate, build, and dry-run without sending signed bundles.
- `live`: signs EIP-1559 executor transactions and submits bundles to configured relays.

### Live Mode Requirements

- `EXECUTE_MODE=live`
- `PRIVATE_KEY` present (executor transaction signer)
- `FLASHBOTS_SIGNING_KEY` present (bundle auth signer)
- Deployed and configured contracts (`FlashArbitrage`, `MultiDexRouter`) on target chain

The launcher and node configuration fail fast if required live credentials are missing.

### Current Readiness

- ✅ End-to-end pipeline: ingest → classify → detect → simulate → build → relay handling
- ✅ Preflight relay simulation (`eth_callBundle`) before live submission
- ✅ Real-time metrics and dashboard for operational visibility
- ⚠️ Not audited for real funds; run with testnet/simulation defaults unless you accept production risk

> Recommended workflow: validate strategy changes in `simulate`, then promote selectively to `live` with audited contracts and strict key management.
