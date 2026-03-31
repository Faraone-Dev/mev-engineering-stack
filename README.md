# MEV Protocol

**High-Performance Multi-Language MEV Engineering Stack**

Sub-microsecond mempool monitoring, transaction classification, bundle construction, and relay submission targeting **Arbitrum**. Four-language architecture where each layer uses the optimal tool for its domain.

![Rust](https://img.shields.io/badge/Rust-Core_Engine-orange?style=flat-square&logo=rust)
![Go](https://img.shields.io/badge/Go-Network_Layer-00ADD8?style=flat-square&logo=go)
![C](https://img.shields.io/badge/C-Hot_Path-A8B9CC?style=flat-square&logo=c)
![Solidity](https://img.shields.io/badge/Solidity-Contracts-363636?style=flat-square&logo=solidity)

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
| **contracts/** | Solidity | Flash loan arbitrage (Balancer V2), multi-DEX routing | `src/FlashArbitrage.sol` |
| **proto/** | Protocol Buffers | Cross-language service contract (Go ↔ Rust) | `mev.proto` |

---

## Live Dashboard

Real-time monitoring dashboard polling Prometheus metrics every 2 seconds. Single self-contained HTML file — no build tools, no dependencies.

```
┌──────────────┬──────────────────────────────────────────────┬──────────────┐
│  NETWORK     │        TRANSACTION PROCESSING PIPELINE       │  LIVE FEED   │
│              │  Ingest → Classify → Filter → Opp → Relay   │  35 events   │
│  Block #447M │         9.4K    9.4K   1.3K   1.3K   0      │              │
│  RPC 3/3     │                                              │  PERFORMANCE │
│  Propagation │        REVENUE & P&L           SESSION       │  40.7ns      │
│  708ms       │        Total Extracted: 0.0000 ETH           │  425ns       │
│              │                                              │  4 workers   │
│  TX SOURCE   │        CLASSIFICATION BREAKDOWN              │              │
│  Classified  │   V2: 26  V3: 360  Transfer: 920            │  ERRORS      │
│  9.4K        │                                              │  0  0  0     │
│              │        EIP-1559 GAS ORACLE   250ms           │              │
│  Buffer 0%   │   Base: 0.02  Priority: 2.0  Pred: 0.017    │              │
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
# Open directly in browser
open dashboard/index.html
# Requires the Go node running with Prometheus on :9091
```

---

## Benchmark Results

Measured with [Criterion 0.5](https://bheisler.github.io/criterion.rs/book/) on Intel i5-8250U @ 1.60GHz. Production targets co-located bare-metal.

### Rust Core — `cargo bench`

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

### Go Network — `go test -bench .`

| Operation | Latency | Allocs | Throughput |
|-----------|---------|--------|------------|
| Tx classification (selector dispatch) | **40.7 ns/op** | 0 B / 0 alloc | ~24.5M tx/sec |
| EIP-1559 base fee calculation | **425 ns/op** | 152 B / 6 alloc | ~2.3M/sec |

> Full pipeline processes a transaction **1500× faster** than Arbitrum's 250ms block time.

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

- **revm 8.0** — local EVM simulation without forked geth
- **Tokio 1.35** — async multi-threaded runtime
- **crossbeam** — lock-free channels for detector→simulator pipeline
- **alloy + ethers** — type-safe Ethereum primitives and ABI encoding
- **Prometheus** — `metrics-exporter-prometheus` for hot-path instrumentation
- **tonic + prost** — gRPC server exposing detection pipeline to Go

```bash
cd core
cargo build --release     # opt-level=3, lto=fat, codegen-units=1
cargo test                # 170 tests (137 unit + 10 integration + 23 proptest)
cargo bench               # 7 Criterion benchmark groups
```

### Detection Pipeline

```
PendingTx → parse_swap() → ArbitrageDetector  ──┐
            (8 selectors,   BackrunDetector   ────┼─▶ EvmSimulator ──▶ BundleBuilder ──▶ Bundle
             checked math)  LiquidationDetector─┘    (V2 + V3 AMM)     (ABI encode)
```

- **ArbitrageDetector**: Cross-DEX price discrepancy with cached pool state, checked arithmetic
- **BackrunDetector**: Price recovery capture after large swaps with impact threshold
- **LiquidationDetector**: Aave V3, Compound V3, Morpho position tracking with close factor limits
- **MultiThreaded**: Parallel worker pool via crossbeam channels
- **EvmSimulator**: V2 constant-product (35 ns) + V3 concentrated liquidity via `sqrtPriceX96` → virtual reserves conversion, auto-routing by pool type

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

Foundry-based Solidity with hardened callback validation. 5-field execution context prevents callback forgery (executor, token, amount, swapHash, nonce). Factory-verified pool addresses.

| Contract | Purpose | Security |
|----------|---------|----------|
| **FlashArbitrage.sol** | Balancer V2 flash loan → multi-DEX execution | Callback bound to (executor, token, amount, swap hash) |
| **MultiDexRouter.sol** | Aggregated routing across V2/V3/Sushi/Curve | Trusted factory/router whitelist, strict ERC20 checks |

```bash
cd contracts
forge build
forge test -vvv
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
git clone https://github.com/ivanpiardi/mev-engineering-stack.git
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
| **Rust core** | **170 tests** (137 unit + 10 integration + 23 proptest) | `cargo test` + proptest + Criterion | See breakdown below |
| **Go network** | 23 tests, 2 benchmarks | `go test` | Config parsing, EIP-1559 oracle, tx classification (V2/V3 selectors), multi-relay strategies |
| **Rust bench** | 7 groups | Criterion 0.5 | Full pipeline, keccak, AMM, pool lookup, ABI, U256, crossbeam |
| **Solidity** | Foundry suite | `forge test` | Flash arbitrage execution, multi-DEX routing, callback validation |
| **C hot path** | `make test` | Custom runner | Keccak correctness, RLP encoding, SIMD validation |

### Rust Core — 170 Tests Breakdown

| Module | Tests | Coverage |
|--------|-------|----------|
| `detector/arbitrage` | 26 | V2/V3/V3-output calldata parsing, ABI word decoders, `decode_addr`, `dex_from_fee`, `match_pool_to_swap`, profit calc (profitable/gas-exceeds/no-arb), truncated calldata |
| `detector/backrun` | 3 | Swap selectors, price impact, small-swap filter |
| `detector/liquidation` | 4 | Liquidation detection, healthy skip, close factor, stale pruning |
| `detector/multi_threaded` | 1 | Parallel swap simulation |
| `simulator` | 19 | Constant-product math (happy/zero/overflow/fee=100%), pool cache (load/update/get/reserves fallback), simulate (arb/backrun/liquidation/bundle), success rate |
| `builder` | 16 | ABI encoding, all 3 bundle types, swap path (2-hop/empty/missing pool), no-contract error, count |
| `config` | 9 | Serde roundtrip, save/reload, `from_env` fallback, chain defaults, strategy, performance |
| `ffi/hot_path` | 24 | Keccak-256 known vectors, function selectors (`transfer`/`approve`/V2 swap), `address_eq`, RLP encoding, `OpportunityQueue` FIFO, `TxBuffer` cap, `SwapInfoFFI` |
| `ffi` | 3 | Keccak fallback, RLP single byte, RLP short string |
| `grpc/server` | 3 | `bytes_to_u128` edge cases |
| `mempool/ultra_ws` | 11 | Tx hash extraction, swap classification (V2/V3/Universal Router), non-swap rejection |
| `arbitrum/pools` | 12 | AMM `get_amount_out` (basic/reverse/zero/high-fee), `get_price`, token list validation |
| `types` | 8 | `estimate_gas` for all DexType × OpportunityType combinations |
| `proptest` | 23 | Constant-product invariants (7), ABI roundtrip (3), gas bounds (5), keccak (3), data structures (2), swap selectors (1), overflow safety (2) |
| `integration` | 10 | Full pipeline end-to-end, engine lifecycle, all bundle types, simulator count |

### Bugs Found & Fixed

| Bug | Severity | Module | Fix |
|-----|----------|--------|-----|
| `is_likely_swap()` — OR condition classified all non-zero-first-byte txs as swaps | **Critical** | `mempool/ultra_ws` | Removed `\|\| (selector[0] != 0x00)` |
| `constant_product_swap()` — unchecked u128 arithmetic silently overflowed on whale trades | **High** | `simulator` | `checked_mul`/`checked_add`, returns 0 on overflow |
| Proptest tested local copy of AMM function, not production code | **High** | `tests/proptest` | Now imports `mev_core::simulator::constant_product_swap` directly |

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| **4 languages** | Go for concurrent network I/O, Rust for safe high-perf compute, C for SIMD hot paths, Solidity for on-chain. Mirrors production MEV infra. |
| **gRPC over FFI for Go↔Rust** | cgo pins goroutines to OS threads, defeating Go's scheduler. gRPC gives process separation, independent scaling, and proto-defined contracts. |
| **revm over forked geth** | Pure Rust, no cgo dependency, fork-mode simulation, deterministic gas. 10-50× faster for single-tx sim. |
| **Balancer flash loans** | 0% fee vs Aave's 0.09%. When margins are basis points, eliminating the fee is critical. |
| **Constant-product fast filter** | x·y=k math at 35 ns screens candidates before expensive revm simulation. Only analytical survivors hit EVM. |
| **Arbitrum-first** | 10-100× cheaper gas, 250ms blocks, less MEV competition. Proof-of-concept before L1. |
| **Monitor-only fallback** | Go node degrades gracefully when Rust core is offline — keeps logging rather than crashing. |

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
├── contracts/              # Solidity — FlashArbitrage, MultiDexRouter
│   ├── src/                # Contract source + interfaces
│   └── test/               # Foundry test suite
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
