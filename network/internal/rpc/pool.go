package rpc

import (
	"context"
	"sync"
	"time"

	"github.com/ethereum/go-ethereum/ethclient"
	"github.com/mev-protocol/network/internal/metrics"
	"github.com/rs/zerolog/log"
)

// Config for RPC pool
type Config struct {
	Endpoints           []string
	MaxConns            int
	RequestTimeout      time.Duration
	ReconnectDelay      time.Duration
	HealthCheckInterval time.Duration
}

// Client wraps an eth client with metadata
type Client struct {
	*ethclient.Client
	endpoint string
	latency  time.Duration
	healthy  bool
}

// Pool manages multiple RPC connections
type Pool struct {
	config  Config
	clients []*Client
	mu      sync.RWMutex
	idx     int
	running bool
	wg      sync.WaitGroup
}

// NewPool creates a new RPC pool
func NewPool(cfg Config) *Pool {
	return &Pool{
		config:  cfg,
		clients: make([]*Client, 0, len(cfg.Endpoints)),
	}
}

// Start initializes all connections
func (p *Pool) Start(ctx context.Context) error {
	log.Info().Int("endpoints", len(p.config.Endpoints)).Msg("Starting RPC pool")

	p.mu.Lock()
	p.running = true
	p.mu.Unlock()

	// Connect to all endpoints
	for _, endpoint := range p.config.Endpoints {
		client, err := p.connect(ctx, endpoint)
		if err != nil {
			log.Warn().Err(err).Str("endpoint", endpoint).Msg("Failed to connect")
			continue
		}
		p.clients = append(p.clients, client)
	}

	if len(p.clients) == 0 {
		log.Error().Msg("No RPC connections available")
	}

	// Start health checker
	p.wg.Add(1)
	go p.healthCheckLoop(ctx)

	return nil
}

// Stop closes all connections
func (p *Pool) Stop(ctx context.Context) {
	p.mu.Lock()
	p.running = false
	p.mu.Unlock()

	log.Info().Msg("Stopping RPC pool")

	for _, client := range p.clients {
		client.Close()
	}

	p.wg.Wait()
}

// GetClient returns the best available client
func (p *Pool) GetClient() (*Client, error) {
	p.mu.RLock()
	defer p.mu.RUnlock()

	if len(p.clients) == 0 {
		return nil, ErrNoClients
	}

	// Find healthiest client with lowest latency
	var best *Client
	for _, c := range p.clients {
		if !c.healthy {
			continue
		}
		if best == nil || c.latency < best.latency {
			best = c
		}
	}

	if best == nil {
		// Fallback to any client
		return p.clients[0], nil
	}

	return best, nil
}

// GetWSClient returns a WebSocket client
func (p *Pool) GetWSClient() (*Client, error) {
	p.mu.RLock()
	defer p.mu.RUnlock()

	// Find a WS client
	for _, c := range p.clients {
		if c.healthy {
			return c, nil
		}
	}

	if len(p.clients) > 0 {
		return p.clients[0], nil
	}

	return nil, ErrNoClients
}

func (p *Pool) connect(ctx context.Context, endpoint string) (*Client, error) {
	start := time.Now()

	client, err := ethclient.DialContext(ctx, endpoint)
	if err != nil {
		return nil, err
	}

	latency := time.Since(start)

	log.Info().
		Str("endpoint", endpoint).
		Dur("latency", latency).
		Msg("Connected to RPC")

	return &Client{
		Client:   client,
		endpoint: endpoint,
		latency:  latency,
		healthy:  true,
	}, nil
}

func (p *Pool) healthCheckLoop(ctx context.Context) {
	defer p.wg.Done()

	ticker := time.NewTicker(p.config.HealthCheckInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			p.checkHealth(ctx)
		}
	}
}

func (p *Pool) checkHealth(ctx context.Context) {
	p.mu.Lock()
	defer p.mu.Unlock()

	for _, client := range p.clients {
		start := time.Now()

		// Simple health check: get block number
		checkCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		_, err := client.BlockNumber(checkCtx)
		cancel()

		if err != nil {
			client.healthy = false
			metrics.RPCRequestsTotal.WithLabelValues(client.endpoint, "error").Inc()
			log.Warn().
				Str("endpoint", client.endpoint).
				Err(err).
				Msg("RPC health check failed")
		} else {
			client.healthy = true
			client.latency = time.Since(start)
			metrics.RPCRequestsTotal.WithLabelValues(client.endpoint, "ok").Inc()
			metrics.RPCLatency.WithLabelValues(client.endpoint).Observe(client.latency.Seconds())
		}
	}

	// Count healthy endpoints
	healthy := 0
	for _, c := range p.clients {
		if c.healthy {
			healthy++
		}
	}
	metrics.RPCHealthyEndpoints.Set(float64(healthy))
}

// Custom errors
type PoolError string

func (e PoolError) Error() string { return string(e) }

const (
	ErrNoClients PoolError = "no RPC clients available"
)
