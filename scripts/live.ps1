# ═══════════════════════════════════════════════════════════════════════════════
# MEV PROTOCOL — LIVE LAUNCHER
# Single command: .\scripts\live.ps1
# Starts: Go node (metrics) + Rust engine (simulation) + Dashboard
# ═══════════════════════════════════════════════════════════════════════════════

param(
    [ValidateSet("mainnet","testnet")]
    [string]$Network = "mainnet",
    [switch]$NoDashboard,
    [switch]$BuildFirst
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent (Split-Path -Parent $PSCommandPath)

# ── Load .env ────────────────────────────────────────────────────────────────
$envFile = Join-Path $ROOT ".env"
if (Test-Path $envFile) {
    Get-Content $envFile | ForEach-Object {
        if ($_ -match '^\s*([^#][^=]+?)\s*=\s*(.+?)\s*$') {
            [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        }
    }
}

# ── Banner ───────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  ╔═══════════════════════════════════════════════════════════╗" -ForegroundColor Blue
Write-Host "  ║                                                           ║" -ForegroundColor Blue
Write-Host "  ║     ███╗   ███╗███████╗██╗   ██╗                          ║" -ForegroundColor Cyan
Write-Host "  ║     ████╗ ████║██╔════╝██║   ██║                          ║" -ForegroundColor Cyan
Write-Host "  ║     ██╔████╔██║█████╗  ██║   ██║                          ║" -ForegroundColor Cyan
Write-Host "  ║     ██║╚██╔╝██║██╔══╝  ╚██╗ ██╔╝                          ║" -ForegroundColor Cyan
Write-Host "  ║     ██║ ╚═╝ ██║███████╗ ╚████╔╝                           ║" -ForegroundColor Cyan
Write-Host "  ║     ╚═╝     ╚═╝╚══════╝  ╚═══╝                            ║" -ForegroundColor Cyan
Write-Host "  ║                                                           ║" -ForegroundColor Blue
Write-Host "  ║     P R O T O C O L  —  L I V E   E N G I N E            ║" -ForegroundColor White
Write-Host "  ║                                                           ║" -ForegroundColor Blue
Write-Host "  ╚═══════════════════════════════════════════════════════════╝" -ForegroundColor Blue
Write-Host ""

# ── Network config ───────────────────────────────────────────────────────────
if ($Network -eq "mainnet") {
    $wsUrl  = $env:ARBITRUM_WS_URL
    $chain  = "Arbitrum One (42161)"
    if (-not $wsUrl) {
        Write-Host "  [!] ARBITRUM_WS_URL not set in .env" -ForegroundColor Red
        exit 1
    }
} else {
    $wsUrl  = $env:ARBITRUM_WS_URL -replace "arb-mainnet", "arb-sepolia"
    $chain  = "Arbitrum Sepolia (421614)"
}

Write-Host "  Network:    $chain" -ForegroundColor Yellow
Write-Host "  Mode:       SIMULATION (read-only, no execution)" -ForegroundColor Green
Write-Host "  Dashboard:  http://localhost — metrics on :9091" -ForegroundColor Gray
Write-Host ""

# ── Optional build ───────────────────────────────────────────────────────────
if ($BuildFirst) {
    Write-Host "  [1/4] Building Rust core..." -ForegroundColor Green
    Push-Location (Join-Path $ROOT "core")
    cargo build --release 2>&1 | Select-Object -Last 3
    Pop-Location

    Write-Host "  [2/4] Building Go node..." -ForegroundColor Green
    Push-Location (Join-Path $ROOT "network")
    go build -o (Join-Path $ROOT "bin\mev-node.exe") ./cmd/mev-node/ 2>&1
    Pop-Location

    Write-Host "  [3/4] Build complete" -ForegroundColor Green
    Write-Host ""
}

# ── Kill old processes ───────────────────────────────────────────────────────
Stop-Process -Name "mev-node" -Force -ErrorAction SilentlyContinue
Stop-Process -Name "mev_launcher" -Force -ErrorAction SilentlyContinue

# ── Start Go node (metrics + dashboard backend) ─────────────────────────────
Write-Host "  [>] Starting Go network node..." -ForegroundColor Cyan

$env:MEV_RPC_ENDPOINTS   = $wsUrl
$env:MEV_MEMPOOL_MIN_VALUE = "0"
$env:MEV_MEMPOOL_FILTER   = "false"
$env:MEV_METRICS_ENABLED  = "true"
$env:MEV_METRICS_ADDR     = ":9091"

$goJob = Start-Job -ScriptBlock {
    param($networkDir, $wsUrl)
    Set-Location $networkDir
    $env:MEV_RPC_ENDPOINTS    = $wsUrl
    $env:MEV_MEMPOOL_MIN_VALUE = "0"
    $env:MEV_MEMPOOL_FILTER    = "false"
    $env:MEV_METRICS_ENABLED   = "true"
    $env:MEV_METRICS_ADDR      = ":9091"
    go run ./cmd/mev-node/ 2>&1
} -ArgumentList (Join-Path $ROOT "network"), $wsUrl

Start-Sleep -Seconds 3
Write-Host "  [+] Go node started (PID: $($goJob.Id))" -ForegroundColor Green

# ── Start Rust engine (simulation) ──────────────────────────────────────────
Write-Host "  [>] Starting Rust MEV engine..." -ForegroundColor Cyan

$env:EXECUTE_MODE = "simulate"

$rustJob = Start-Job -ScriptBlock {
    param($coreDir, $envFile)
    Set-Location $coreDir
    # Load .env into this process
    if (Test-Path $envFile) {
        Get-Content $envFile | ForEach-Object {
            if ($_ -match '^\s*([^#][^=]+?)\s*=\s*(.+?)\s*$') {
                [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
            }
        }
    }
    $env:EXECUTE_MODE = "simulate"
    cargo run --release --bin mev_launcher 2>&1
} -ArgumentList (Join-Path $ROOT "core"), $envFile

Start-Sleep -Seconds 2
Write-Host "  [+] Rust engine started (PID: $($rustJob.Id))" -ForegroundColor Green

# ── Open dashboard ───────────────────────────────────────────────────────────
if (-not $NoDashboard) {
    $dashboardPath = Join-Path $ROOT "dashboard\index.html"
    Write-Host "  [>] Opening dashboard..." -ForegroundColor Cyan
    Start-Process $dashboardPath
    Write-Host "  [+] Dashboard opened in browser" -ForegroundColor Green
}

# ── Status ───────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  ═══════════════════════════════════════════════════════════" -ForegroundColor DarkGray
Write-Host "  All systems running. Press Ctrl+C to stop." -ForegroundColor White
Write-Host "  ═══════════════════════════════════════════════════════════" -ForegroundColor DarkGray
Write-Host ""
Write-Host "  Go node logs:   Receive-Job -Id $($goJob.Id)" -ForegroundColor DarkGray
Write-Host "  Rust logs:      Receive-Job -Id $($rustJob.Id)" -ForegroundColor DarkGray
Write-Host ""

# ── Stream logs ──────────────────────────────────────────────────────────────
try {
    while ($true) {
        # Print Go node output
        $goOutput = Receive-Job -Id $goJob.Id -ErrorAction SilentlyContinue
        if ($goOutput) {
            $goOutput | ForEach-Object {
                Write-Host "  [GO]   $_" -ForegroundColor DarkCyan
            }
        }

        # Print Rust output
        $rustOutput = Receive-Job -Id $rustJob.Id -ErrorAction SilentlyContinue
        if ($rustOutput) {
            $rustOutput | ForEach-Object {
                Write-Host "  [RUST] $_" -ForegroundColor DarkYellow
            }
        }

        Start-Sleep -Milliseconds 500
    }
}
finally {
    # Cleanup on Ctrl+C
    Write-Host ""
    Write-Host "  Shutting down..." -ForegroundColor Yellow
    Stop-Job -Id $goJob.Id -ErrorAction SilentlyContinue
    Stop-Job -Id $rustJob.Id -ErrorAction SilentlyContinue
    Remove-Job -Id $goJob.Id -Force -ErrorAction SilentlyContinue
    Remove-Job -Id $rustJob.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Name "mev-node" -Force -ErrorAction SilentlyContinue
    Write-Host "  All processes stopped." -ForegroundColor Green
}
