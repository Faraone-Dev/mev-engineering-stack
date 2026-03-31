# mev-core

**Rust MEV Detection Engine** — sub-microsecond opportunity detection, EVM simulation, and bundle construction.

## Build

```bash
cargo build --release     # opt-level=3, lto=fat, codegen-units=1
cargo test                # unit + property-based tests
cargo bench               # Criterion benchmarks (7 groups)
```

Release profile: `opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`.

## Architecture

```
                          ┌──────────────────────────────┐
                          │         MevEngine            │
                          │         (lib.rs)             │
                          └──────┬─────────┬─────────┬───┘
                                 │         │         │
                    ┌────────────┘         │         └────────────┐
                    ▼                      ▼                      ▼
           ┌────────────────┐    ┌──────────────────┐   ┌─────────────────┐
           │ Detector       │    │ EvmSimulator     │   │ BundleBuilder   │
           │                │    │                  │   │                 │
           │ Arbitrage      │───▶│ V2 const_product │──▶│ encode_arb_call │
           │ Backrun        │    │ V3 concentrated  │   │ encode_liq_call │
           │ Liquidation    │    │ auto-route by    │   │ swap_path encode│
           │ MultiThreaded  │    │ pool type        │   │ Flashbots format│
           └────────┬───────┘    └──────────────────┘   └─────────────────┘
                    │
                    │ FFI (optional)
                    ▼
           ┌────────────────┐
           │ fast/ (C)      │
           │ keccak, RLP    │
           │ SIMD, queue    │
           └────────────────┘
```

## Modules

| Module | Path | Description |
|--------|------|-------------|
| **detector** | `src/detector/` | Opportunity detection pipeline |
| ├ arbitrage | `detector/arbitrage.rs` | Cross-DEX price discrepancy — 8 V2/V3 selectors, cached pool state |
| ├ backrun | `detector/backrun.rs` | Price recovery capture after large swaps |
| ├ liquidation | `detector/liquidation.rs` | Under-collateralized position liquidation (Aave V3, Compound V3, Morpho) |
| └ multi_threaded | `detector/multi_threaded.rs` | Parallel worker pool with crossbeam channels |
| **simulator** | `src/simulator/` | EVM simulation |
| | | V2 constant-product x·y=k fast filter (35 ns) + V3 concentrated liquidity swap (`sqrtPriceX96` → virtual reserves, auto-routing by `is_v3` flag) + revm 8.0 fork validation. Checked arithmetic on all paths. |
| **builder** | `src/builder/` | Bundle construction |
| | | ABI encoding, swap path packing, gas pricing, Flashbots-compatible format |
| **grpc** | `src/grpc/` | gRPC server (tonic 0.11) |
| | | `DetectOpportunity` (unary: classify → detect → simulate → build), `StreamOpportunities` (server-streaming via `tokio::broadcast` with `min_profit` filter + lag recovery), `GetStatus` (uptime + detection count) |
| **ffi** | `src/ffi/` | C FFI bindings with pure-Rust fallback |
| | | Keccak-256 (alignment-safe), RLP, SIMD batch price impact (`__uint128_t`), lock-free queue (CAS slot-claim), arena allocator. **Graceful degradation**: compiles and runs without C toolchain via Rust fallback implementations. |
| **arbitrum** | `src/arbitrum/` | Arbitrum L2 engine (~1700 lines) |
| | | **Pool discovery**: 3 DEX (Uniswap V3, SushiSwap, Camelot), 24 token pairs, all V3 fee tiers (100/500/3000/10000), `sqrtPriceX96` → virtual reserve conversion, parallel refresh via `join_all` |
| | | **Detection**: 2-hop + triangular arbitrage, binary search for optimal amount (64 iterations), gas estimation per DEX type, near-miss debug logging |
| | | **Execution**: Balancer V2 flash loan executor, EIP-1559 signing (chain_id=42161), pre-send simulation, slippage protection, multi-DEX calldata encoding |
| **mempool** | `src/mempool/` | WebSocket ultra-latency transaction polling |
| **config** | `src/config.rs` | Typed configuration with chain/strategy/performance sections |
| **types** | `src/types.rs` | Shared pipeline types: `PendingTx → SwapInfo → Opportunity → SimulationResult → Bundle` |

## Binaries

| Binary | Source | Purpose |
|--------|--------|---------|
| `mev-engine` | `src/main.rs` | Main MEV extraction engine |
| `scanner` | `src/bin/scanner.rs` | Standalone transaction scanner |
| `benchmark` | `src/bin/benchmark.rs` | Custom latency benchmarks |

## Benchmark Results

Criterion 0.5 on Intel i5-8250U @ 1.60GHz. Run `cargo bench` to reproduce.

| Group | Benchmark | Latency |
|-------|-----------|---------|
| crypto | keccak256/20 | 515 ns |
| crypto | keccak256/32 | 552 ns |
| crypto | keccak256/64 | 554 ns |
| crypto | keccak256/256 | 1.09 µs |
| amm | constant_product/0.1 ETH | 35 ns |
| amm | constant_product/1 ETH | 35 ns |
| amm | constant_product/10 ETH | 35 ns |
| amm | two_hop_arbitrage | 69 ns |
| lookup | pool_lookup_10k | 55 ns |
| abi | encode_address | 17 ns |
| abi | encode_u256 | 17 ns |
| abi | encode_swap_path_3hop | 53 ns |
| **pipeline** | **detect → simulate → build** | **608 ns** |
| u256 | mul_div | 83 ns |
| u256 | add_sub | 0.6 ns |
| u256 | compare | 0.6 ns |
| channel | crossbeam_send_recv | 23 ns |

HTML reports: `target/criterion/report/index.html`

## Tests

**170 tests** — full coverage across all modules. Run with `cargo test`.

```
test result: ok. 137 passed (unit) + 10 passed (integration) + 23 passed (proptest) = 170 total
```

### Unit Tests (137)

| Module | Tests | What's Verified |
|--------|-------|-----------------|
| `detector/arbitrage` | 26 | V2/V3/V3-output calldata parsing, ABI word decoders (`u128`, `u64`, `u32`, `usize`), `decode_addr` (with/without `0x`, empty, invalid hex), `dex_from_fee` dispatch, `match_pool_to_swap` (forward/reversed/no-match), profit calculation (profitable, gas-exceeds, no-arb, single-price), truncated calldata rejection |
| `detector/backrun` | 3 | Swap selector matching, price impact calculation, small-swap filtering |
| `detector/liquidation` | 4 | Liquidatable position detection, healthy skip, close factor limits, stale pruning |
| `detector/multi_threaded` | 1 | Parallel swap simulation |
| `simulator` | 19 | Constant-product math (happy path, zero reserves, fee=100%, u128 overflow), pool cache (`load_pools`, `update_pool`, `get_pool`, `pool_reserves` cache hit/fallback/zero-addr), `ordered_pair` canonical ordering, `simulate` (arbitrage, backrun, liquidation), `simulate_bundle`, `success_rate`, `estimate_tx_gas`, simulation count tracking |
| `builder` | 16 | ABI encoding (`address` with/without `0x`, empty, `u256` zero/max), arbitrage/backrun/liquidation bundle construction, swap path encoding (2-hop, empty, missing pool fallback), build without contract → error, build count increment |
| `config` | 9 | Serde JSON roundtrip (full config + chain config), save/reload to disk, `from_env` fallback to defaults, Ethereum/Arbitrum chain presence, strategy defaults, performance non-zero |
| `ffi/hot_path` | 24 | Keccak-256 known vectors (empty → `0xc5d246...`, "hello" → `0x1c8aff...`), function selectors (`transfer` = `0xa9059cbb`, `approve` = `0x095ea7b3`, V2 swap), `address_eq` (same/different/zero), RLP encoding (address length+prefix, u256 zero/small/large), `calc_price_impact_batch` (basic + zero-reserve), `OpportunityQueue` (new/push/pop/empty/FIFO order), `TxBuffer` (new/empty/write-read/512 cap), `SwapInfoFFI` default |
| `ffi` | 3 | Keccak fallback, RLP encode single byte, RLP encode short string |
| `grpc/server` | 3 | `bytes_to_u128` (empty, 1 ETH, 32-byte input) |
| `mempool/ultra_ws` | 11 | Tx hash extraction (correct H256 value, missing result, truncated, invalid hex), swap classification (V2 `swapExactTokensForTokens`, V3 `exactInputSingle`, Universal Router `execute`), non-swap rejection (`approve`, `transfer`, empty input, short input) |
| `arbitrum/pools` | 12 | `get_amount_out` (basic, reverse direction, zero reserves, zero input, high fee comparison), `get_price` (basic, zero reserve), token list (non-empty, WETH first, no duplicate addresses) |
| `types` | 8 | `estimate_gas` across all `DexType` × `OpportunityType` combinations: 2×V2 arb (341k), V2+V3 mixed (371k), backrun V3 (171k), liquidation empty (101k), Curve (301k), Balancer (281k), Sushi==V2, 3-hop triangular (491k) |
| `bench/latency` | 1 | Benchmark framework validation |
| `lib` | 1 | Engine creation and state |

### Integration Tests (10)

| Test | What's Verified |
|------|-----------------|
| `engine_starts_and_stops_cleanly` | Full lifecycle: start → stop without panic |
| `engine_stats_start_at_zero` | Counter initialization |
| `arbitrage_opportunity_simulates_with_gas` | Arbitrage → simulate → gas > 0 |
| `arbitrage_builds_valid_bundle` | Arbitrage → build → valid tx with correct selector |
| `backrun_builds_valid_bundle` | Backrun → build → correct priority fee |
| `liquidation_builds_valid_bundle` | Liquidation → build → correct calldata layout |
| `builder_handles_all_opportunity_types` | All 3 types produce valid bundles |
| `full_pipeline_arbitrage_end_to_end` | detect → simulate → build (complete pipeline) |
| `liquidation_opportunity_simulates` | Liquidation sim produces non-zero gas |
| `simulator_tracks_count_across_calls` | Atomic counter consistency |

### Property Tests (23 — proptest)

| Property | Invariant |
|----------|-----------|
| `swap_zero_input_zero_output` | 0 in → 0 out (∀ reserves, fees) |
| `swap_zero_reserves_zero_output` | Empty pool → 0 out |
| `swap_output_bounded_by_reserve` | Output < reserve_out (conservation) |
| `swap_monotonically_increasing` | More input → more output |
| `swap_higher_fee_less_output` | Higher fee → less output |
| `swap_preserves_k_invariant` | x·y ≥ k after swap (constant product) |
| `swap_no_panic_on_overflow` | u128::MAX/2 inputs — no panic, returns 0 |
| `swap_fee_100_percent_returns_zero` | 10000 bps → zero output |
| `abi_u256_roundtrip` | Encode → decode identity |
| `abi_u256_always_32_bytes` | Output is always exactly 32 bytes |
| `abi_address_padding` | First 12 bytes always zero |
| `gas_estimate_minimum` | Gas ≥ 21000 base |
| `gas_estimate_capped_by_limit` | Gas ≤ limit |
| `gas_increases_with_nonzero_bytes` | More non-zero calldata → more gas |
| `keccak_deterministic` | Same input → same hash |
| `keccak_collision_resistant` | Different input → different hash |
| `keccak_output_always_32_bytes` | Output is always 32 bytes |
| `unknown_selector_not_classified` | Random 4-byte selector → not a known swap |
| `dashmap_insert_get_consistent` | Concurrent map consistency |
| `crossbeam_channel_fifo` | Channel preserves ordering |
| `estimate_gas_arb_includes_flash_loan` | Arbitrage gas ≥ 101k (includes flash overhead) |
| `estimate_gas_backrun_no_flash_loan` | Backrun gas has no flash loan overhead |
| `estimate_gas_monotonic_with_hops` | More hops → more gas |

### Bugs Found & Fixed by Tests

| Bug | Severity | Fix |
|-----|----------|-----|
| `is_likely_swap()` had `\|\| (selector[0] != 0x00)` — classified ALL non-zero-first-byte txs as swaps | **Critical** | Removed spurious OR condition; now only matches known swap selectors |
| `constant_product_swap()` used unchecked arithmetic — silent u128 overflow on whale trades | **High** | Added `checked_mul`/`checked_add`; returns 0 on overflow instead of wrapping |
| Proptest tested a LOCAL COPY of `constant_product_swap` with checked math, while production code had unchecked math | **High** | Proptest now imports `mev_core::simulator::constant_product_swap` directly |

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| tokio | 1.35 | Async runtime (full features) |
| revm | 8.0 | EVM simulation (std, serde) |
| alloy | 0.1 | Ethereum type primitives |
| ethers | 2.0 | ABI generation, WebSocket |
| tonic | 0.11 | gRPC server |
| prost | 0.12 | Protocol buffer codegen |
| tiny-keccak | 2.0 | Keccak-256 hashing |
| dashmap | 5.5 | Concurrent hash map |
| crossbeam-channel | 0.5 | Lock-free MPSC channels |
| metrics | 0.22 | Prometheus metrics |
| criterion | 0.5 | Benchmarking (dev) |
| proptest | 1.4 | Property-based testing (dev) |

## Type Flow

```
PendingTx              Raw mempool transaction (hash, from, to, calldata, gas)
    │
    ▼ parse_swap()
SwapInfo               Decoded swap (dex, token_in, token_out, amount, fee)
    │
    ▼ detect()
Opportunity            MEV opportunity (type, path, expected_profit, gas_estimate)
    │
    ▼ simulate()
SimulationResult       EVM result (success, actual_profit, gas_used, state_changes)
    │
    ▼ build()
Bundle                 Flashbots bundle (txs, target_block, timing constraints)
```
