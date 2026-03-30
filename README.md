# MEV Protocol

**High-Performance Multi-Language MEV Engineering Stack**

Low-latency mempool monitoring, transaction classification, bundle construction, and relay submission for **Arbitrum**. Four-language architecture optimized for each layer of the MEV pipeline.

![Rust](https://img.shields.io/badge/Rust-Core_Engine-orange?style=flat-square&logo=rust)
![Go](https://img.shields.io/badge/Go-Network_Layer-00ADD8?style=flat-square&logo=go)
![C](https://img.shields.io/badge/C-Hot_Path-A8B9CC?style=flat-square&logo=c)
![Solidity](https://img.shields.io/badge/Solidity-Contracts-363636?style=flat-square&logo=solidity)
![CI](https://img.shields.io/github/actions/workflow/status/ivanpiardi/mev-engineering-stack/ci.yml?style=flat-square&label=CI)

---

## Architecture

```
                    ┌──────────────────────────────────────────────┐
                    │              MEV Protocol Stack               │
                    └──────────────────────────────────────────────┘
                                        │
          ┌─────────────────────────────┼─────────────────────────────┐
          │                             │                             │
    ┌─────┴─────┐               ┌──────┴──────┐              ┌──────┴──────┐
    │  network/  │               │    core/     │              │ contracts/  │
    │    (Go)    │               │   (Rust)     │              │ (Solidity)  │
    │            │               │              │              │             │
    │ Mempool    │──tx stream──▶│ Detection    │──bundles───▶│ FlashArb    │
    │ Pipeline   │               │ Simulation   │              │ MultiDex    │
    │ Relay      │◀─submission──│ Optimization │              │ Callbacks   │
    │ Metrics    │               │ Benchmark    │              │             │
    └────────────┘               └──────┬───────┘              └─────────────┘
                                        │
                                  ┌─────┴─────┐
                                  │   fast/    │
                                  │    (C)     │
                                  │            │
                                  │ Keccak     │
                                  │ RLP Encode │
                                  │ SIMD Utils │
                                  │ Lock-free Q│
                                  │ Mem Pool   │
                                  └────────────┘
```

### Layer Breakdown

| Layer | Language | Purpose | Key Files |
|-------|----------|---------|-----------|
| **network/** | Go 1.21 | Mempool subscription, tx classification, relay submission, Prometheus metrics | `cmd/mev-node/main.go` |
| **core/** | Rust | MEV detection engine, EVM simulation (revm), Arbitrum scanner | `src/main.rs`, `src/detector/`, `src/simulator/` |
| **contracts/** | Solidity | Flash loan arbitrage, multi-DEX routing, callback hardening | `src/FlashArbitrage.sol`, `src/MultiDexRouter.sol` |
| **fast/** | C | SIMD keccak, RLP encoding, lock-free queue, memory pool | `src/keccak.c`, `src/lockfree_queue.c` |

---

## Go Network Node — `network/`

Production-grade mempool monitoring and relay infrastructure.

### Components

| Package | Description |
|---------|-------------|
| `internal/mempool` | WebSocket pending tx subscription via `gethclient`, configurable selector filtering |
| `internal/pipeline` | Multi-worker tx classifier — UniswapV2 (6 selectors), V3 (4), ERC20, Aave liquidations, flash loans |
| `internal/block` | New head subscription with reorg detection (configurable depth), polling fallback |
| `internal/gas` | EIP-1559 base fee oracle — real formula implementation with multi-block prediction |
| `internal/relay` | Flashbots bundle submission + multi-relay manager (Race / Primary / All strategies) |
| `internal/rpc` | Connection pool with health checking, latency-based routing, automatic reconnection |
| `internal/metrics` | Prometheus instrumentation — RPC latency, mempool throughput, pipeline classification, relay stats |
| `pkg/config` | Environment-based configuration with typed parsing and sensible defaults |

### Build & Test

```bash
cd network
go build ./...
go test ./... -v          # 23 tests
go test ./... -bench .    # benchmarks
```

### Benchmarks

Measured on Intel i5-8250U @ 1.60GHz (laptop). Server-grade hardware would be faster.

| Operation | Latency | Allocs | Throughput |
|-----------|---------|--------|------------|
| Tx classification (selector lookup) | **40.7 ns/op** | 0 B / 0 alloc | ~24.5M tx/sec |
| EIP-1559 base fee calculation | **425 ns/op** | 152 B / 6 alloc | ~2.3M/sec |

The classification pipeline processes transactions **1500x faster** than Arbitrum's block production rate.

### Pipeline Flow

```
Mempool (gethclient) → Buffer (10k) → Workers (4x) → Classify → Decode → Output
                                                         │
                                          ┌──────────────┼──────────────┐
                                          │              │              │
                                       SwapV2         SwapV3      Liquidation
                                    (6 selectors)  (4 selectors)   (Aave V2/V3)
```

---

## Rust Core Engine — `core/`

High-performance MEV detection and simulation.

- **revm** for local EVM simulation of arbitrage paths
- **Tokio** async runtime with multi-threaded executor
- **crossbeam** lock-free channels for detector ↔ simulator pipeline
- **alloy + ethers** for Ethereum type primitives and ABI encoding
- **Prometheus metrics** via `metrics-exporter-prometheus`
- **C FFI** integration for hot-path keccak and RLP operations (`fast/`)

### Build & Test

```bash
cd core
cargo build --release     # opt-level=3, lto=fat, codegen-units=1
cargo test                # 33 unit tests
cargo bench               # criterion benchmarks
```

### Benchmarks (Rust Core)

Measured on the same Intel i5-8250U. Production would target co-located bare-metal.

| Operation | Latency | Notes |
|-----------|---------|-------|
| Arbitrage detection (V2+V3 parsing + pool lookup) | **~800 ns** | 8 swap selectors, cached pool state |
| Backrun impact estimation (constant-product) | **~350 ns** | Price impact + recovery model |
| Liquidation scan (3 protocols × position set) | **~1.2 µs** | Aave V3, Compound V3, Morpho |
| EVM simulation (constant-product AMM) | **~2.5 µs** | Per-opportunity-type strategies |
| Bundle ABI encoding | **~400 ns** | Proper Solidity ABI with dynamic bytes |
| Full pipeline (detect → simulate → bundle) | **~5 µs** | End-to-end per transaction |

### Go ↔ Rust Integration (gRPC)

The Go network layer forwards classified transactions to the Rust core via **gRPC** (tonic/prost on Rust, google.golang.org/grpc on Go).

```
Go Pipeline → ClassifiedTx (proto) → gRPC → Rust Detector → Simulator → Bundle Builder
                                                                              │
                                                          DetectionResult ◀───┘
                                                          (opportunities + pre-built bundles)
```

- **Proto definition**: `proto/mev.proto` — `MevEngine` service with `DetectOpportunity`, `StreamOpportunities`, `GetStatus` RPCs
- **Rust server**: `core/src/grpc/server.rs` — full pipeline (decode → detect → simulate → build → respond)
- **Go client**: `network/internal/strategy/client.go` — 100ms timeout, keepalive, graceful fallback to monitor-only mode
- **Latency budget**: Target < 10ms round-trip for detect+simulate+bundle on co-located infra

---

## Smart Contracts — `contracts/`

Foundry-based Solidity contracts with comprehensive security hardening.

### Contracts

- **FlashArbitrage.sol** — Balancer flash loan arbitrage with V2/V3 execution paths
- **MultiDexRouter.sol** — Multi-DEX routing with trusted factory/router validation

### Security Hardening

- Flash loan callback bound to active execution context (executor, token, amount, swap hash)
- UniswapV3 callbacks accepted only from verified pool for active swap
- Malformed route decoding rejection
- Trusted router/factory whitelisting
- Strict ERC20 return-data checks on transfer/transferFrom/approve

### Test

```bash
cd contracts
forge test -vvv
```

---

## C Hot Path — `fast/`

Low-level performance-critical components linked into the Rust core via FFI.

| File | Purpose |
|------|---------|
| `keccak.c` | Keccak-256 hashing |
| `rlp.c` | RLP encoding for Ethereum transactions |
| `simd_utils.c` | SIMD-accelerated byte operations |
| `lockfree_queue.c` | Lock-free MPSC queue for cross-thread data flow |
| `memory_pool.c` | Arena allocator for zero-alloc transaction processing |
| `parser.c` | Binary data parsing utilities |

```bash
cd fast
make            # build static library
make test       # run test_all
```

---

## Quick Start

```bash
# 1. Clone
git clone https://github.com/ivanpiardi/mev-engineering-stack.git
cd mev-engineering-stack

# 2. Configure
cp .env.example .env
# Edit .env with your RPC endpoints and keys

# 3. Build all layers
cd network && go build ./... && cd ..
cd core && cargo build --release && cd ..
cd contracts && forge build && cd ..
cd fast && make && cd ..

# 4. Run Go network node
cd network && go run ./cmd/mev-node/

# 5. Run tests
cd network && go test ./...
cd core && cargo test
cd contracts && forge test
```

### Environment

Copy `.env.example` to `.env` at the project root. All components read from this file.

Key variables:
- `MEV_RPC_ENDPOINTS` — Comma-separated WebSocket RPC URLs (Go node)
- `ARBITRUM_RPC_URL` / `ARBITRUM_WS_URL` — Arbitrum endpoints (Rust core)
- `FLASHBOTS_SIGNING_KEY` — Bundle signing key for relay submission
- `MEV_PIPELINE_WORKERS` — Number of parallel classification workers (default: 4)
- `MEV_METRICS_ADDR` — Prometheus metrics endpoint (default: `:9090`)

See [.env.example](.env.example) for the full list.

---

## CI / Quality Gates

- **GitHub Actions**: `.github/workflows/ci.yml` — Rust, Go, Solidity build + test
- **Local**: `make build`, `make test`, `make lint`, `make ci-local`
- **Security**: `.env` gitignored, sanitized `.env.example`, callback spoofing tests

## Post-Deploy Checklist

1. Set whitelisted executors on FlashArbitrage
2. Set trusted V2 routers and V3 factory
3. Set trusted factories on MultiDexRouter
4. Keep contracts paused until dry-run simulation passes
5. Verify Prometheus metrics are reporting correctly

---

## Project Structure

```
mev-engineering-stack/
├── .env.example            # Environment template
├── .github/workflows/      # CI pipeline
├── contracts/              # Solidity — FlashArbitrage, MultiDexRouter, Foundry tests
├── core/                   # Rust — MEV engine, scanner, revm simulation
│   └── src/
│       ├── detector/       # Opportunity detection
│       ├── simulator/      # Local EVM simulation
│       ├── mempool/        # Mempool data handling
│       ├── builder/        # Bundle construction
│       ├── grpc/           # gRPC server (tonic) — Go integration
│       ├── arbitrum/       # Arbitrum-specific logic
│       └── ffi/            # C FFI bindings
├── fast/                   # C — keccak, RLP, SIMD, lock-free queue, memory pool
│   ├── src/                # Implementation
│   ├── include/            # Headers
│   └── test/               # Test suite
├── network/                # Go — mempool monitor, pipeline, relay, metrics
│   ├── cmd/mev-node/       # Entry point
│   ├── internal/           # Core packages (block, gas, mempool, pipeline, relay, rpc, metrics)
│   └── pkg/                # Public packages (config, types)
├── proto/                  # gRPC service definition (mev.proto)
├── dashboard/              # Real-time monitoring dashboard (HTML/JS)
├── config/                 # Chain configs (arbitrum.json, dex.json, tokens.json)
├── scripts/                # Build and deploy scripts
└── docker/                 # Container runtime
```

---

## Dashboard

Real-time Grafana-style monitoring dashboard served alongside the Go metrics endpoint.

- 3-column layout: Network I/O, MEV Engine, System Health
- Animated pipeline visualization with live tx flow
- Ring gauges for detection rate, simulation success, bundle inclusion
- Live feed of classified transactions and detected opportunities

Open `dashboard/index.html` or access via the metrics server at `/dashboard`.

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| **4 languages (Go + Rust + C + Solidity)** | Each layer uses the optimal tool: Go for concurrent network I/O, Rust for safe high-perf compute, C for SIMD/lock-free hot paths, Solidity for on-chain execution. This mirrors production MEV infra at firms like Flashbots, Skip, and Jump. |
| **gRPC over FFI for Go↔Rust** | FFI (cgo) would pin goroutines to OS threads, defeating Go's scheduler. gRPC gives clean cross-process separation, independent scaling, and proto-defined contracts. Latency overhead (~0.5ms) is acceptable for the mempool→detection path. |
| **revm over geth-based simulation** | revm is pure Rust with no cgo dependency, supports fork-mode simulation, and gives deterministic gas accounting. 10-50x faster than forked geth for single-tx simulation. |
| **Balancer flash loans over Aave** | Balancer V2 Vault flash loans have 0% fee (vs Aave's 0.09%). For arbitrage where profit margins are basis points, eliminating the flash loan fee is critical. |
| **Constant-product AMM model in simulator** | Real revm fork simulation is used for final validation, but the fast-path detector uses x*y=k math for sub-microsecond screening. Only opportunities passing the analytical filter hit the expensive EVM simulation. |
| **Arbitrum-first targeting** | Lower gas costs (10-100x cheaper than L1), faster block times (250ms), and less MEV competition than Ethereum mainnet. Ideal for proving the system before L1 deployment. |
| **C hot path via FFI** | Keccak-256 and RLP encoding are called millions of times per second. C with AVX2 SIMD provides 2-5x speedup over Rust's `sha3` crate for batch operations. Lock-free queue avoids mutex contention in the detector→simulator pipeline. |
| **Monitor-only fallback** | Go node degrades gracefully when Rust core is offline — continues logging classified txs instead of crashing. Critical for operational resilience. |

---

## Test Coverage

| Layer | Tests | Type |
|-------|-------|------|
| **Go network** | 23 tests, 2 benchmarks | Unit + integration |
| **Rust core** | 33 tests | Unit (detector, simulator, builder, gRPC, FFI) |
| **Solidity** | Foundry test suite | Fork + fuzz tests |
| **C hot path** | `make test` | Correctness + SIMD validation |

---

## License

Proprietary. See [LICENSE](LICENSE) for details.
