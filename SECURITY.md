# Security Policy

## Supported Use

This repository is intended for engineering research and education. The software is **not audited** and should not be deployed with real funds without independent review.

## Reporting a Vulnerability

Please do not open public issues for undisclosed vulnerabilities.

Instead, report privately to project maintainers with:

- Affected component and file path
- Impact summary (data loss, fund loss, denial of service, etc.)
- Reproduction steps (minimal test case preferred)
- Suggested mitigation (if available)

We aim to acknowledge reports within **48 hours** and provide a fix or mitigation plan within **7 days**.

## Threat Model

### What We Protect

| Asset | Threat | Mitigation |
|-------|--------|------------|
| **Signing keys** | Exfiltration via logs, env leaks, or repo commits | Keys loaded from `.env` only; never logged; `.gitignore` enforced |
| **Bundle content** | Front-running by relays or observers | EIP-191 signed payloads; Flashbots private relay; no plaintext broadcast |
| **RPC credentials** | Credential stuffing, leaked API keys | Loaded from environment; placeholder values in committed config |
| **Smart contract funds** | Callback spoofing, re-entrancy, flash loan manipulation | `msg.sender` validation, execution context isolation, nonce tracking |
| **Pipeline integrity** | Malformed calldata, overflow, panic-induced downtime | Bounds checking, `checked_mul`/`checked_add`, proptest fuzzing |

### Attack Surface by Layer

| Layer | Surface | Key Controls |
|-------|---------|-------------|
| **Solidity contracts** | On-chain execution, callback validation | Balancer vault check, executor whitelist, pause mechanism |
| **Rust core** | Calldata parsing, FFI boundary, gRPC input | Length validation before slice access, typed deserialization |
| **Go network** | WebSocket input, RPC responses, relay submission | Selector-based filtering, context timeouts, TLS for RPC |
| **C hot path** | Memory management, SIMD operations | Arena allocator bounds, CAS-only concurrency (no mutexes) |

## Known Issues

| Issue | Severity | Status | Mitigation |
|-------|----------|--------|------------|
| Flash arbitrage contract uses hardcoded Balancer vault address | Medium | Open | Verify address per chain before deployment |
| Calldata parser assumes well-formed ABI encoding | Low | Open | Returns `None`/error code for malformed input; no panic |
| Lock-free queue CAS loop has no backoff under high contention | Low | Open | Production workloads stay below contention threshold |

## Audit Status

This codebase has **not been formally audited**. The following self-review measures are in place:

- 170+ automated tests including property-based fuzzing (proptest)
- Foundry fuzz testing (10k runs default, 50k CI) for smart contracts
- `cargo clippy -D warnings` enforced on all Rust code
- Solidity callback validation and access control reviewed against common attack patterns

A formal audit is recommended before any mainnet deployment with real funds.

## Secret Handling

- **Never** commit private keys, tokens, RPC credentials, or signed payloads.
- Use `config/.env.example` as template only — copy to `.env` and populate locally.
- Keep runtime secrets in local `.env` files excluded by `.gitignore`.
- Signing keys should be rotated regularly and scoped to testnet during development.

## Pre-Deployment Hardening Checklist

Before deploying to any chain with real value:

1. **Build & test** — `make build && make test && make lint` pass cleanly.
2. **Config review** — No placeholder values (`YOUR_KEY`) remain in active config.
3. **Secret scan** — `git log --all -p | grep -i "private\|secret\|0x[0-9a-f]{64}"` returns no matches.
4. **Contract verification** — Deployed bytecode matches source via `forge verify-contract`.
5. **Pause state** — Contracts deploy in paused state; unpause only after manual inspection.
6. **Relay authentication** — Flashbots signing key is fresh and not shared with other systems.
7. **Rate limits** — RPC provider rate limits configured to prevent accidental billing spikes.
8. **Monitoring** — Prometheus + Grafana dashboards active; alerts configured for anomalies.
9. **Dry run** — Execute full pipeline on testnet with `--submit` flag before mainnet.
10. **Rollback plan** — Document how to pause contracts and stop the pipeline within 30 seconds.
