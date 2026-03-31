// MEV Protocol Network Node
//
// Entry point for the Go network layer. Initializes and orchestrates:
//   - RPC connection pool with health-checking and latency routing
//   - Block watcher with reorg detection
//   - EIP-1559 gas oracle with multi-block prediction
//   - Mempool monitor (WebSocket pending tx subscription)
//   - Transaction classification pipeline (V2/V3 swaps, liquidations, flash loans)
//   - Flashbots relay manager (race / primary / all strategies)
//   - gRPC client forwarding classified txs to the Rust MEV core
//
// All components start in dependency order and shut down gracefully on SIGINT/SIGTERM.
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
	"github.com/mev-protocol/network/internal/strategy"
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

	// Record node start time
	metrics.NodeStartTime.Set(float64(time.Now().Unix()))

	// Track uptime in background
	go func() {
		tick := time.NewTicker(time.Second)
		defer tick.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-tick.C:
				metrics.NodeUptimeSeconds.Inc()
			}
		}
	}()

	// Initialize core components
	rpcPool := rpc.NewPool(cfg.RPC)
	blockWatcher := block.NewWatcher(cfg.Block, rpcPool)
	gasOracle := gas.NewOracle(cfg.Gas, rpcPool, blockWatcher)
	mempoolMonitor := mempool.NewMonitor(cfg.Mempool, rpcPool)

	// Merge tx feeds: mempool pending txs + block-extracted txs.
	// On chains without a public mempool (Arbitrum, Optimism) the mempool
	// channel is effectively empty, so the block-based feed is the primary source.
	mergedTxChan := mergeTxChans(mempoolMonitor.TxChan(), blockWatcher.BlockTxChan())
	txPipeline := pipeline.NewPipeline(cfg.Pipeline, mergedTxChan)

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

// consumePipeline reads classified transactions from the pipeline and
// forwards them to the Rust MEV core via gRPC for opportunity detection.
// Detected opportunities are logged and bundles are relayed.
func consumePipeline(
	ctx context.Context,
	p *pipeline.Pipeline,
	gasOracle *gas.Oracle,
	blockWatcher *block.Watcher,
	relayMgr *relay.Manager,
) {
	// Connect to Rust core gRPC server
	strategyClient, err := strategy.NewClient(strategy.DefaultConfig())
	if err != nil {
		log.Warn().Err(err).Msg("Rust core gRPC unavailable — running in monitor-only mode")
		consumePipelineMonitorOnly(ctx, p, gasOracle, blockWatcher)
		return
	}
	defer strategyClient.Close()
	log.Info().Msg("Connected to Rust MEV core via gRPC")

	for {
		select {
		case <-ctx.Done():
			sent, recv, errs := strategyClient.Stats()
			log.Info().
				Uint64("sent", sent).
				Uint64("opportunities", recv).
				Uint64("errors", errs).
				Msg("Strategy client shutting down")
			return

		case tx, ok := <-p.OutputChan():
			if !ok {
				return
			}

			// Prepare target block and base fee from live oracles
			var baseFeeBytes []byte
			if estimate := gasOracle.GetEstimate(); estimate != nil && estimate.BaseFee != nil {
				baseFeeBytes = estimate.BaseFee.Bytes()
			}
			targetBlock := blockWatcher.TargetBlock()

			// Build gRPC request from classified tx
			var toBytes []byte
			if tx.Tx.To != nil {
				toBytes = tx.Tx.To.Bytes()
			}

			reqCtx, reqCancel := context.WithTimeout(ctx, 100*time.Millisecond)
			result, err := strategyClient.DetectOpportunity(
				reqCtx,
				tx.Tx.Hash.Bytes(),
				tx.Tx.From.Bytes(),
				toBytes,
				tx.Tx.Value,
				tx.Tx.GasPrice,
				tx.Tx.GasLimit,
				tx.Tx.Input,
				tx.Tx.Nonce,
				int32(tx.Class),
				targetBlock,
				baseFeeBytes,
			)
			reqCancel()

			if err != nil {
				log.Debug().Err(err).Str("hash", tx.Tx.Hash.Hex()).Msg("Detection RPC failed")
				continue
			}

			// Log any detected opportunities
			strategy.LogOpportunities(result, tx.Tx.Hash.Hex())

			// If bundles were built, submit to relay
			if result.Found {
				for _, opp := range result.Opportunities {
					if opp.Bundle != nil && len(opp.Bundle.Transactions) > 0 {
						log.Info().
							Str("hash", tx.Tx.Hash.Hex()).
							Int("bundleTxs", len(opp.Bundle.Transactions)).
							Uint64("targetBlock", opp.Bundle.TargetBlock).
							Msg("Bundle ready for relay submission")
						// relayMgr.SubmitBundle(ctx, opp.Bundle) — wired when live
					}
				}
			}
		}
	}
}

// consumePipelineMonitorOnly is the fallback when Rust core is unavailable.
// Logs classified transactions without attempting MEV detection.
func consumePipelineMonitorOnly(
	ctx context.Context,
	p *pipeline.Pipeline,
	gasOracle *gas.Oracle,
	blockWatcher *block.Watcher,
) {
	for {
		select {
		case <-ctx.Done():
			return
		case tx, ok := <-p.OutputChan():
			if !ok {
				return
			}
			logEvent := log.Debug().
				Str("hash", tx.Tx.Hash.Hex()).
				Int("class", int(tx.Class))
			if estimate := gasOracle.GetEstimate(); estimate != nil && estimate.BaseFee != nil {
				logEvent = logEvent.Str("baseFee", estimate.BaseFee.String())
			}
			if target := blockWatcher.TargetBlock(); target > 0 {
				logEvent = logEvent.Uint64("targetBlock", target)
			}
			logEvent.Msg("Classified tx (monitor-only, Rust core offline)")
		}
	}
}

// mergeTxChans fans-in two PendingTx channels into one.
func mergeTxChans(a, b <-chan *mempool.PendingTx) <-chan *mempool.PendingTx {
	out := make(chan *mempool.PendingTx, 10000)
	forward := func(ch <-chan *mempool.PendingTx) {
		for tx := range ch {
			out <- tx
		}
	}
	go forward(a)
	go forward(b)
	return out
}
