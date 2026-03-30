package relay

import (
	"context"
	"sync"
	"sync/atomic"
	"time"

	"github.com/mev-protocol/network/internal/metrics"
	"github.com/rs/zerolog/log"
)

// RelayType identifies a relay provider
type RelayType string

const (
	RelayFlashbots RelayType = "flashbots"
	RelayBloXroute RelayType = "bloxroute"
	RelayMEVShare  RelayType = "mev-share"
)

// Relay defines the interface for bundle relay providers
type Relay interface {
	Name() RelayType
	SendBundle(ctx context.Context, bundle *Bundle) (*BundleResponse, error)
	SimulateBundle(ctx context.Context, bundle *Bundle) (*SimulationResult, error)
}

// MultiConfig for the multi-relay manager
type MultiConfig struct {
	Strategy      SubmitStrategy
	MaxConcurrent int
	SubmitTimeout time.Duration
	// Only submit to relays where simulation is profitable
	RequireSimulation bool
	MinProfitWei      int64
}

// SubmitStrategy determines how bundles are sent to relays
type SubmitStrategy int

const (
	// StrategyRace sends to all relays concurrently, uses first success
	StrategyRace SubmitStrategy = iota
	// StrategyPrimary sends to primary relay, fallback on failure
	StrategyPrimary
	// StrategyAll sends to all relays and collects all results
	StrategyAll
)

// SubmitResult aggregates results from multi-relay submission
type SubmitResult struct {
	Relay    RelayType
	Response *BundleResponse
	Error    error
	Latency  time.Duration
}

// Manager handles bundle submission across multiple relay providers
type Manager struct {
	config  MultiConfig
	relays  []Relay
	primary Relay

	submitted atomic.Uint64
	succeeded atomic.Uint64
	failed    atomic.Uint64

	mu sync.RWMutex
}

// NewManager creates a multi-relay manager
func NewManager(cfg MultiConfig) *Manager {
	return &Manager{
		config: cfg,
		relays: make([]Relay, 0),
	}
}

// AddRelay adds a relay provider to the manager
func (m *Manager) AddRelay(r Relay, primary bool) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.relays = append(m.relays, r)
	if primary {
		m.primary = r
	}
}

// SubmitBundle sends a bundle according to the configured strategy
func (m *Manager) SubmitBundle(ctx context.Context, bundle *Bundle) ([]*SubmitResult, error) {
	m.mu.RLock()
	relays := m.relays
	m.mu.RUnlock()

	if len(relays) == 0 {
		return nil, ErrNoRelays
	}

	m.submitted.Add(1)

	switch m.config.Strategy {
	case StrategyRace:
		return m.submitRace(ctx, bundle, relays)
	case StrategyPrimary:
		return m.submitPrimary(ctx, bundle, relays)
	case StrategyAll:
		return m.submitAll(ctx, bundle, relays)
	default:
		return m.submitAll(ctx, bundle, relays)
	}
}

// submitRace sends to all relays concurrently, returns on first success
func (m *Manager) submitRace(ctx context.Context, bundle *Bundle, relays []Relay) ([]*SubmitResult, error) {
	ctx, cancel := context.WithTimeout(ctx, m.config.SubmitTimeout)
	defer cancel()

	resultChan := make(chan *SubmitResult, len(relays))

	for _, r := range relays {
		go func(relay Relay) {
			start := time.Now()
			resp, err := relay.SendBundle(ctx, bundle)
			resultChan <- &SubmitResult{
				Relay:    relay.Name(),
				Response: resp,
				Error:    err,
				Latency:  time.Since(start),
			}
		}(r)
	}

	// Wait for first success or all failures
	var results []*SubmitResult
	for i := 0; i < len(relays); i++ {
		result := <-resultChan
		results = append(results, result)
		m.recordResult(result)

		if result.Error == nil {
			m.succeeded.Add(1)
			log.Info().
				Str("relay", string(result.Relay)).
				Dur("latency", result.Latency).
				Str("hash", result.Response.BundleHash).
				Msg("Bundle accepted (race winner)")
			return results, nil
		}
	}

	m.failed.Add(1)
	return results, ErrAllRelaysFailed
}

// submitPrimary sends to primary relay, falls back to others on failure
func (m *Manager) submitPrimary(ctx context.Context, bundle *Bundle, relays []Relay) ([]*SubmitResult, error) {
	var results []*SubmitResult

	// Try primary first
	if m.primary != nil {
		start := time.Now()
		resp, err := m.primary.SendBundle(ctx, bundle)
		result := &SubmitResult{
			Relay:    m.primary.Name(),
			Response: resp,
			Error:    err,
			Latency:  time.Since(start),
		}
		results = append(results, result)
		m.recordResult(result)

		if err == nil {
			m.succeeded.Add(1)
			return results, nil
		}

		log.Warn().
			Str("relay", string(m.primary.Name())).
			Err(err).
			Msg("Primary relay failed, trying fallbacks")
	}

	// Fallback to other relays sequentially
	for _, r := range relays {
		if m.primary != nil && r.Name() == m.primary.Name() {
			continue
		}

		start := time.Now()
		resp, err := r.SendBundle(ctx, bundle)
		result := &SubmitResult{
			Relay:    r.Name(),
			Response: resp,
			Error:    err,
			Latency:  time.Since(start),
		}
		results = append(results, result)
		m.recordResult(result)

		if err == nil {
			m.succeeded.Add(1)
			return results, nil
		}
	}

	m.failed.Add(1)
	return results, ErrAllRelaysFailed
}

// submitAll sends to all relays and collects all results
func (m *Manager) submitAll(ctx context.Context, bundle *Bundle, relays []Relay) ([]*SubmitResult, error) {
	ctx, cancel := context.WithTimeout(ctx, m.config.SubmitTimeout)
	defer cancel()

	var (
		wg      sync.WaitGroup
		mu      sync.Mutex
		results []*SubmitResult
	)

	for _, r := range relays {
		wg.Add(1)
		go func(relay Relay) {
			defer wg.Done()

			start := time.Now()
			resp, err := relay.SendBundle(ctx, bundle)
			result := &SubmitResult{
				Relay:    relay.Name(),
				Response: resp,
				Error:    err,
				Latency:  time.Since(start),
			}

			mu.Lock()
			results = append(results, result)
			mu.Unlock()

			m.recordResult(result)
		}(r)
	}

	wg.Wait()

	// Check if at least one succeeded
	anySuccess := false
	for _, r := range results {
		if r.Error == nil {
			anySuccess = true
			break
		}
	}

	if anySuccess {
		m.succeeded.Add(1)
	} else {
		m.failed.Add(1)
	}

	return results, nil
}

func (m *Manager) recordResult(result *SubmitResult) {
	status := "success"
	if result.Error != nil {
		status = "error"
	}

	metrics.RelayBundlesSubmitted.WithLabelValues(string(result.Relay), status).Inc()
	metrics.RelaySubmitLatency.WithLabelValues(string(result.Relay)).Observe(result.Latency.Seconds())
}

// Stats returns relay submission statistics
func (m *Manager) Stats() (submitted, succeeded, failed uint64) {
	return m.submitted.Load(), m.succeeded.Load(), m.failed.Load()
}

// Errors
type RelayError string

func (e RelayError) Error() string { return string(e) }

const (
	ErrNoRelays        RelayError = "no relay providers configured"
	ErrAllRelaysFailed RelayError = "all relay submissions failed"
)
