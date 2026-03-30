package main

import (
	"context"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/mev-protocol/network/internal/block"
	"github.com/mev-protocol/network/internal/gas"
	"github.com/mev-protocol/network/internal/mempool"
	"github.com/mev-protocol/network/internal/metrics"
	"github.com/mev-protocol/network/internal/pipeline"
	"github.com/mev-protocol/network/internal/relay"
	"github.com/mev-protocol/network/internal/rpc"
	"github.com/mev-protocol/network/pkg/config"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
)

const version = "0.2.0"

func main() {
	// Setup structured logging
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnixMs
	log.Logger = log.Output(zerolog.ConsoleWriter{Out: os.Stderr, TimeFormat: "15:04:05.000"})

	log.Info().
		Str("version", version).
		Msg("MEV Protocol Network Node")

	// Load configuration from environment
	cfg, err := config.Load()
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to load config")
	}

	// Create root context with cancellation
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// Start Prometheus metrics server
	if cfg.Metrics.Enabled {
		metrics.ServeMetrics(cfg.Metrics.Addr)
	}

	// Initialize core components
	rpcPool := rpc.NewPool(cfg.RPC)
	blockWatcher := block.NewWatcher(cfg.Block, rpcPool)
	gasOracle := gas.NewOracle(cfg.Gas, rpcPool, blockWatcher)
	mempoolMonitor := mempool.NewMonitor(cfg.Mempool, rpcPool)
	txPipeline := pipeline.NewPipeline(cfg.Pipeline, mempoolMonitor.TxChan())

	// Initialize relay layer
	flashbotsRelay := relay.NewFlashbots(cfg.Relay)
	relayManager := relay.NewManager(cfg.Multi)
	relayManager.AddRelay(flashbotsRelay, true)

	// Start all components in dependency order
	components := []struct {
		name  string
		start func(context.Context) error
	}{
		{"rpc-pool", rpcPool.Start},
		{"block-watcher", blockWatcher.Start},
		{"gas-oracle", gasOracle.Start},
		{"mempool-monitor", mempoolMonitor.Start},
		{"tx-pipeline", txPipeline.Start},
		{"flashbots-relay", flashbotsRelay.Start},
	}

	for _, c := range components {
		if err := c.start(ctx); err != nil {
			log.Fatal().Err(err).Str("component", c.name).Msg("Failed to start")
		}
		log.Info().Str("component", c.name).Msg("Started")
	}

	log.Info().
		Int("rpcEndpoints", len(cfg.RPC.Endpoints)).
		Int("pipelineWorkers", cfg.Pipeline.Workers).
		Bool("metricsEnabled", cfg.Metrics.Enabled).
		Msg("All components started — node is ready")

	// Consume pipeline output (classified transactions)
	go consumePipeline(ctx, txPipeline, gasOracle, blockWatcher, relayManager)

	// Wait for shutdown signal
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	sig := <-sigChan
	log.Info().Str("signal", sig.String()).Msg("Shutdown signal received")

	// Graceful shutdown with timeout (reverse order)
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer shutdownCancel()

	cancel() // Cancel root context first

	txPipeline.Stop(shutdownCtx)
	mempoolMonitor.Stop(shutdownCtx)
	gasOracle.Stop(shutdownCtx)
	blockWatcher.Stop(shutdownCtx)
	flashbotsRelay.Stop(shutdownCtx)
	rpcPool.Stop(shutdownCtx)

	log.Info().Msg("Shutdown complete")
}

// consumePipeline reads classified transactions from the pipeline.
// In production, this is where MEV strategy execution would be wired in.
// The pipeline provides fully classified and decoded swap transactions
// ready for opportunity detection by the Rust core via FFI or gRPC.
func consumePipeline(
	ctx context.Context,
	p *pipeline.Pipeline,
	gasOracle *gas.Oracle,
	blockWatcher *block.Watcher,
	relayMgr *relay.Manager,
) {
	for {
		select {
		case <-ctx.Done():
			return

		case tx, ok := <-p.OutputChan():
			if !ok {
				return
			}

			// Log classified transaction with gas context
			logEvent := log.Debug().
				Str("hash", tx.Tx.Hash.Hex()).
				Int("class", int(tx.Class))

			if estimate := gasOracle.GetEstimate(); estimate != nil && estimate.BaseFee != nil {
				logEvent = logEvent.Str("baseFee", estimate.BaseFee.String())
			}

			if target := blockWatcher.TargetBlock(); target > 0 {
				logEvent = logEvent.Uint64("targetBlock", target)
			}

			logEvent.Msg("Classified tx ready for strategy engine")

			// In production: forward to Rust core via FFI for opportunity detection
			// The architectural boundary is here — Go handles network I/O,
			// Rust handles compute-intensive MEV detection
			_ = relayMgr // Available for bundle submission when strategy engine responds
		}
	}
}
