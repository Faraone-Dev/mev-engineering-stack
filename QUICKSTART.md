# MEV Protocol Quick Start

This guide gets the repository building and running locally with reproducible steps.

## 0) Clone and enter workspace

```bash
git clone <your-repository-url>
cd mev-protocol
```

## 1) Prerequisites

Install:

- Rust 1.75+
- Go 1.21+
- Foundry (`forge`)
- GCC/Clang or MinGW toolchain

## 2) Configure environment

From repository root:

### Windows build

```powershell
Copy-Item config/.env.example config/.env
notepad config/.env
```

### Linux/macOS build

```bash
cp config/.env.example config/.env
$EDITOR config/.env
```

Fill required fields (private key, RPC endpoints, relay/auth settings) using your own credentials.

Do not commit populated `.env` files.

## 3) Build all components

### Windows (PowerShell)

```powershell
.\scripts\build.ps1
```

### Linux/macOS

```bash
chmod +x scripts/build.sh
./scripts/build.sh
```

Expected artifacts:

- `core/target/release/mev-engine`
- `bin/mev-node` (or `mev-node.exe` on Windows)
- `contracts/out/`
- `fast/lib/libmev_fast.a`

## 4) Run tests

From repository root:

```bash
make test
```

If `make` is unavailable on Windows, run stack-specific commands:

- `cd fast && mingw32-make test` (or `make test`)
- `cd core && cargo test`
- `cd network && go test ./...`
- `cd contracts && forge test`

## 4.1) Run PR quality checks

```bash
make lint
make ci-local
```

## 5) Run binaries

### Rust engine

```bash
cd core
cargo run --release --bin mev-engine
```

### Go network node

```bash
cd network
go run ./cmd/mev-node
```

## 6) Optional: contracts deployment

Use only after verifying network and key settings:

```bash
./scripts/deploy.sh arbitrum
```

After deployment, apply contract security configuration before live execution:

- `FlashArbitrage.setExecutor(executor, true)`
- `FlashArbitrage.setTrustedV2Router(router, true)` for each approved router
- `FlashArbitrage.setTrustedV3Factory(factory)`
- `MultiDexRouter.setTrustedFactories(v2Factory, v3Factory)`
- `FlashArbitrage.setPaused(false)` only after simulation and dry-run validation

## Troubleshooting

- Build fails in `fast/`: verify C toolchain and `make` availability.
- `forge` missing: run `foundryup` after Foundry installation.
- RPC connection issues: check endpoint permissions and websocket URL correctness.
- Missing env vars: ensure `config/.env` exists and required variables are populated.
- Clippy failures: run `cd core && cargo clippy --all-targets --all-features -- -D warnings` and address warnings before PR.

---

For architecture details and project positioning, see [README.md](README.md).
