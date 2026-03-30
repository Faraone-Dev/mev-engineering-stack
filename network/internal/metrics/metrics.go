package metrics

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/rs/zerolog/log"
)

// RPC metrics
var (
	RPCRequestsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "rpc",
		Name:      "requests_total",
		Help:      "Total RPC requests by endpoint and status",
	}, []string{"endpoint", "status"})

	RPCLatency = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: "mev",
		Subsystem: "rpc",
		Name:      "latency_seconds",
		Help:      "RPC request latency in seconds",
		Buckets:   []float64{0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0},
	}, []string{"endpoint"})

	RPCHealthyEndpoints = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "rpc",
		Name:      "healthy_endpoints",
		Help:      "Number of healthy RPC endpoints",
	})
)

// Mempool metrics
var (
	MempoolTxReceived = promauto.NewCounter(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "mempool",
		Name:      "tx_received_total",
		Help:      "Total transactions received from mempool",
	})

	MempoolTxFiltered = promauto.NewCounter(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "mempool",
		Name:      "tx_filtered_total",
		Help:      "Total transactions that passed filters",
	})

	MempoolTxDropped = promauto.NewCounter(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "mempool",
		Name:      "tx_dropped_total",
		Help:      "Transactions dropped due to full buffer",
	})

	MempoolBufferUsage = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "mempool",
		Name:      "buffer_usage",
		Help:      "Current buffer usage (0.0 - 1.0)",
	})

	MempoolSubscriptionErrors = promauto.NewCounter(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "mempool",
		Name:      "subscription_errors_total",
		Help:      "Total mempool subscription errors",
	})
)

// Block metrics
var (
	BlockLatestNumber = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "block",
		Name:      "latest_number",
		Help:      "Latest observed block number",
	})

	BlockBaseFee = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "block",
		Name:      "base_fee_gwei",
		Help:      "Current base fee in Gwei",
	})

	BlockProcessingLatency = promauto.NewHistogram(prometheus.HistogramOpts{
		Namespace: "mev",
		Subsystem: "block",
		Name:      "processing_latency_seconds",
		Help:      "Time between block timestamp and our observation",
		Buckets:   []float64{0.1, 0.25, 0.5, 1.0, 2.0, 5.0},
	})
)

// Pipeline metrics
var (
	PipelineTxProcessed = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "pipeline",
		Name:      "tx_processed_total",
		Help:      "Transactions processed by stage",
	}, []string{"stage"})

	PipelineStageLatency = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: "mev",
		Subsystem: "pipeline",
		Name:      "stage_latency_seconds",
		Help:      "Processing latency per pipeline stage",
		Buckets:   []float64{0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05},
	}, []string{"stage"})

	PipelineOpportunitiesFound = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "pipeline",
		Name:      "opportunities_found_total",
		Help:      "MEV opportunities identified by type",
	}, []string{"type"})
)

// Relay metrics
var (
	RelayBundlesSubmitted = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: "mev",
		Subsystem: "relay",
		Name:      "bundles_submitted_total",
		Help:      "Total bundles submitted by relay and status",
	}, []string{"relay", "status"})

	RelaySubmitLatency = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: "mev",
		Subsystem: "relay",
		Name:      "submit_latency_seconds",
		Help:      "Bundle submission latency by relay",
		Buckets:   []float64{0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0},
	}, []string{"relay"})

	RelaySimulationProfit = promauto.NewHistogram(prometheus.HistogramOpts{
		Namespace: "mev",
		Subsystem: "relay",
		Name:      "simulation_profit_eth",
		Help:      "Simulated profit per bundle in ETH",
		Buckets:   []float64{0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0},
	})
)

// Gas metrics
var (
	GasBaseFee = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "gas",
		Name:      "base_fee_gwei",
		Help:      "Current EIP-1559 base fee in Gwei",
	})

	GasPriorityFee = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "gas",
		Name:      "priority_fee_gwei",
		Help:      "Suggested priority fee in Gwei",
	})

	GasPredictedBaseFee = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: "mev",
		Subsystem: "gas",
		Name:      "predicted_base_fee_gwei",
		Help:      "Predicted next block base fee in Gwei",
	})
)

// ServeMetrics starts the Prometheus metrics HTTP server
func ServeMetrics(addr string) {
	mux := http.NewServeMux()
	mux.Handle("/metrics", promhttp.Handler())

	server := &http.Server{
		Addr:    addr,
		Handler: mux,
	}

	go func() {
		log.Info().Str("addr", addr).Msg("Metrics server starting")
		if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Error().Err(err).Msg("Metrics server error")
		}
	}()
}
