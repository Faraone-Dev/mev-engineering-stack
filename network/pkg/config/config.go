package config

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/mev-protocol/network/internal/block"
	"github.com/mev-protocol/network/internal/gas"
	"github.com/mev-protocol/network/internal/mempool"
	"github.com/mev-protocol/network/internal/pipeline"
	"github.com/mev-protocol/network/internal/relay"
	"github.com/mev-protocol/network/internal/rpc"
)

// Node holds all configuration for the MEV node
type Node struct {
	RPC      rpc.Config
	Mempool  mempool.Config
	Relay    relay.Config
	Block    block.Config
	Gas      gas.Config
	Pipeline pipeline.Config
	Multi    relay.MultiConfig
	Metrics  MetricsConfig
}

// MetricsConfig for Prometheus endpoint
type MetricsConfig struct {
	Enabled bool
	Addr    string
}

// Load reads configuration from environment variables with sensible defaults
func Load() (*Node, error) {
	endpoints := envStringSlice("MEV_RPC_ENDPOINTS", []string{
		"wss://eth-mainnet.g.alchemy.com/v2/demo",
	})

	if len(endpoints) == 0 {
		return nil, fmt.Errorf("MEV_RPC_ENDPOINTS: at least one endpoint required")
	}

	signingKey := os.Getenv("FLASHBOTS_SIGNING_KEY")

	return &Node{
		RPC: rpc.Config{
			Endpoints:           endpoints,
			MaxConns:            envInt("MEV_RPC_MAX_CONNS", 10),
			RequestTimeout:      envDuration("MEV_RPC_TIMEOUT", 5*time.Second),
			ReconnectDelay:      envDuration("MEV_RPC_RECONNECT_DELAY", time.Second),
			HealthCheckInterval: envDuration("MEV_RPC_HEALTH_INTERVAL", 30*time.Second),
		},
		Mempool: mempool.Config{
			BufferSize:    envInt("MEV_MEMPOOL_BUFFER", 10000),
			FilterEnabled: envBool("MEV_MEMPOOL_FILTER", true),
			MinValue:      envFloat("MEV_MEMPOOL_MIN_VALUE", 1e17), // 0.1 ETH
			TargetSelectors: envStringSlice("MEV_MEMPOOL_SELECTORS", []string{
				"0x38ed1739", // swapExactTokensForTokens
				"0x8803dbee", // swapTokensForExactTokens
				"0x7ff36ab5", // swapExactETHForTokens
				"0x414bf389", // exactInputSingle (UniV3)
				"0xc04b8d59", // exactInput (UniV3)
			}),
		},
		Block: block.Config{
			BufferSize:    envInt("MEV_BLOCK_BUFFER", 16),
			PollInterval:  envDuration("MEV_BLOCK_POLL_INTERVAL", time.Second),
			MaxReorgDepth: envInt("MEV_BLOCK_MAX_REORG", 64),
			TrackBaseFee:  envBool("MEV_BLOCK_TRACK_BASEFEE", true),
		},
		Gas: gas.Config{
			HistorySize:              envInt("MEV_GAS_HISTORY", 20),
			UpdateInterval:           envDuration("MEV_GAS_UPDATE_INTERVAL", 12*time.Second),
			ElasticityMultiplier:     uint64(envInt("MEV_GAS_ELASTICITY", 2)),
			BaseFeeChangeDenominator: uint64(envInt("MEV_GAS_DENOMINATOR", 8)),
		},
		Pipeline: pipeline.Config{
			Workers:         envInt("MEV_PIPELINE_WORKERS", 4),
			ClassifyTimeout: envDuration("MEV_PIPELINE_CLASSIFY_TIMEOUT", 10*time.Millisecond),
			BufferSize:      envInt("MEV_PIPELINE_BUFFER", 5000),
		},
		Relay: relay.Config{
			FlashbotsURL:  envString("MEV_FLASHBOTS_URL", "https://relay.flashbots.net"),
			BloXrouteURL:  envString("MEV_BLOXROUTE_URL", "https://mev.api.blxrbdn.com"),
			SigningKey:    signingKey,
			MaxRetries:    envInt("MEV_RELAY_MAX_RETRIES", 3),
			SubmitTimeout: envDuration("MEV_RELAY_SUBMIT_TIMEOUT", 2*time.Second),
		},
		Multi: relay.MultiConfig{
			Strategy:          relay.StrategyRace,
			MaxConcurrent:     envInt("MEV_RELAY_MAX_CONCURRENT", 3),
			SubmitTimeout:     envDuration("MEV_RELAY_SUBMIT_TIMEOUT", 2*time.Second),
			RequireSimulation: envBool("MEV_RELAY_REQUIRE_SIM", true),
			MinProfitWei:      int64(envInt("MEV_RELAY_MIN_PROFIT", 1000000000000000)), // 0.001 ETH
		},
		Metrics: MetricsConfig{
			Enabled: envBool("MEV_METRICS_ENABLED", true),
			Addr:    envString("MEV_METRICS_ADDR", ":9090"),
		},
	}, nil
}

// Helper functions for environment variable parsing

func envString(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envInt(key string, fallback int) int {
	if v := os.Getenv(key); v != "" {
		if i, err := strconv.Atoi(v); err == nil {
			return i
		}
	}
	return fallback
}

func envFloat(key string, fallback float64) float64 {
	if v := os.Getenv(key); v != "" {
		if f, err := strconv.ParseFloat(v, 64); err == nil {
			return f
		}
	}
	return fallback
}

func envBool(key string, fallback bool) bool {
	if v := os.Getenv(key); v != "" {
		if b, err := strconv.ParseBool(v); err == nil {
			return b
		}
	}
	return fallback
}

func envDuration(key string, fallback time.Duration) time.Duration {
	if v := os.Getenv(key); v != "" {
		if d, err := time.ParseDuration(v); err == nil {
			return d
		}
	}
	return fallback
}

func envStringSlice(key string, fallback []string) []string {
	if v := os.Getenv(key); v != "" {
		parts := strings.Split(v, ",")
		result := make([]string, 0, len(parts))
		for _, p := range parts {
			trimmed := strings.TrimSpace(p)
			if trimmed != "" {
				result = append(result, trimmed)
			}
		}
		if len(result) > 0 {
			return result
		}
	}
	return fallback
}
