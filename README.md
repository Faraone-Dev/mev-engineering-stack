# ⚡ MEV Protocol

**High-Performance Multi-Language MEV Engineering Stack**  
Low-latency detection, simulation, and execution research for EVM ecosystems.

![Rust](https://img.shields.io/badge/Rust-Core-orange?style=for-the-badge&logo=rust)
![Go](https://img.shields.io/badge/Go-Network-00ADD8?style=for-the-badge&logo=go)
![Solidity](https://img.shields.io/badge/Solidity-Contracts-363636?style=for-the-badge&logo=solidity)
![C](https://img.shields.io/badge/C-Hot%20Path-A8B9CC?style=for-the-badge&logo=c)
![CI](https://img.shields.io/badge/CI-Enabled-success?style=for-the-badge)

---

## 🚀 Executive Overview

MEV Protocol is a portfolio-grade systems project built to showcase production-oriented engineering across runtime boundaries.

It combines:

- **Solidity/Yul** for on-chain execution paths
- **Rust** for orchestration, detection, and simulation
- **Go** for mempool networking and relay interaction
- **C** for low-level hot-path components

The repository is structured for technical review, reproducible builds, and iterative extension.

## 🧱 Architecture

- `contracts/` — smart contracts and Foundry tests
- `core/` — Rust engine (`mev-engine`, `scanner`, `benchmark`)
- `network/` — Go node for mempool and relay components
- `fast/` — C static/shared libraries for performance-critical code
- `config/` — chain, DEX, and environment configuration
- `scripts/` — build and deployment scripts
- `docker/` — container runtime assets

## 📌 Engineering Status

- ✅ Multi-stack build and test flow available
- ✅ CI pipeline configured for Rust, Go, and Solidity
- ✅ Contract-layer callback spoofing hardening (Balancer + Uniswap V3)
- ✅ Deterministic route validation with trusted factory/router controls
- ⚠️ Some modules still contain placeholder/TODO logic (notably parts of detector/simulator)

Positioning is intentionally transparent: strong technical foundation with active feature completion.

## 🛠️ Quality & Process

- CI workflow: `.github/workflows/ci.yml`
- Local gates: `make build`, `make test`, `make lint`, `make ci-local`
- Security hygiene: sanitized templates (`config/.env.example`) + strict ignore rules

## 🔐 Contract Hardening Highlights

- Flash loan callback is bound to active execution context (`executor`, `token`, `amount`, `swap hash`).
- Uniswap V3 callbacks are accepted only from the active pool for the active swap.
- Swap route decoding rejects malformed payloads and unknown swap types.
- V2/V3 execution paths validate trusted routers/factories before swap execution.
- ERC20 transfer/transferFrom/approve wrappers enforce strict return-data checks.

## ✅ Post-Deploy Security Checklist

Before enabling execution in production:

1. Set whitelisted executors.
2. Set trusted V2 routers (FlashArbitrage).
3. Set trusted V3 factory (FlashArbitrage).
4. Set trusted V2/V3 factories (MultiDexRouter).
5. Keep contract paused until off-chain simulation and dry-run checks are green.

## ⚡ Quick Start

- Setup guide: [QUICKSTART.md](QUICKSTART.md)
- Contribution guide: [CONTRIBUTING.md](CONTRIBUTING.md)
- Security policy: [SECURITY.md](SECURITY.md)

### Windows (PowerShell)

```powershell
.\scripts\build.ps1
```

### Linux/macOS

```bash
chmod +x scripts/build.sh
./scripts/build.sh
```

## 🎯 Why It Impresses Recruiters

This project demonstrates:

- low-latency architecture and performance tradeoffs
- polyglot systems integration (Rust/Go/C/Solidity)
- smart-contract and off-chain coordination patterns
- mature engineering workflow (CI, templates, quality gates)

## ⚖️ Responsible Use

This repository is for engineering research and education. Users are responsible for legal, compliance, and operational risk management in their jurisdiction.

## 📄 License

Proprietary (as currently configured in project metadata).
