package mempool

import (
	"context"
	"sync"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/ethclient/gethclient"
	"github.com/mev-protocol/network/internal/metrics"
	rpcpool "github.com/mev-protocol/network/internal/rpc"
	"github.com/rs/zerolog/log"
)

// Config for mempool monitor
type Config struct {
	BufferSize      int
	FilterEnabled   bool
	MinValue        float64
	TargetSelectors []string
}

// PendingTx represents a pending transaction
type PendingTx struct {
	Hash      common.Hash
	From      common.Address
	To        *common.Address
	Value     uint64
	GasPrice  uint64
	GasLimit  uint64
	Input     []byte
	Nonce     uint64
	Timestamp time.Time
}

// Monitor watches the mempool for pending transactions
type Monitor struct {
	config    Config
	rpcPool   *rpcpool.Pool
	txChan    chan *PendingTx
	selectors map[string]bool
	mu        sync.RWMutex
	running   bool
	wg        sync.WaitGroup
}

// NewMonitor creates a new mempool monitor
func NewMonitor(cfg Config, pool *rpcpool.Pool) *Monitor {
	selectors := make(map[string]bool)
	for _, sel := range cfg.TargetSelectors {
		selectors[sel] = true
	}

	return &Monitor{
		config:    cfg,
		rpcPool:   pool,
		txChan:    make(chan *PendingTx, cfg.BufferSize),
		selectors: selectors,
	}
}

// Start begins monitoring the mempool
func (m *Monitor) Start(ctx context.Context) error {
	m.mu.Lock()
	m.running = true
	m.mu.Unlock()

	log.Info().Msg("Starting mempool monitor")

	// Start subscription workers
	m.wg.Add(1)
	go m.subscribeLoop(ctx)

	// Start processor
	m.wg.Add(1)
	go m.processLoop(ctx)

	return nil
}

// Stop gracefully stops the monitor
func (m *Monitor) Stop(ctx context.Context) {
	m.mu.Lock()
	m.running = false
	m.mu.Unlock()

	log.Info().Msg("Stopping mempool monitor")
	m.wg.Wait()
}

// TxChan returns the channel for pending transactions
func (m *Monitor) TxChan() <-chan *PendingTx {
	return m.txChan
}

func (m *Monitor) subscribeLoop(ctx context.Context) {
	defer m.wg.Done()

	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		m.mu.RLock()
		if !m.running {
			m.mu.RUnlock()
			return
		}
		m.mu.RUnlock()

		// Subscribe to pending transactions
		if err := m.subscribe(ctx); err != nil {
			log.Error().Err(err).Msg("Subscription error, reconnecting...")
			time.Sleep(time.Second)
		}
	}
}

func (m *Monitor) subscribe(ctx context.Context) error {
	// Get WebSocket client
	client, err := m.rpcPool.GetWSClient()
	if err != nil {
		return err
	}

	// Create a gethclient for pending tx subscription
	// Access the underlying *rpc.Client via the embedded ethclient
	rpcClient := client.Client.Client()
	geth := gethclient.New(rpcClient)

	// Subscribe to pending transaction hashes
	hashChan := make(chan common.Hash, 1000)
	sub, err := geth.SubscribePendingTransactions(ctx, hashChan)
	if err != nil {
		metrics.MempoolSubscriptionErrors.Inc()
		return err
	}
	defer sub.Unsubscribe()

	log.Info().Msg("Subscribed to pending transactions")

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()

		case err := <-sub.Err():
			metrics.MempoolSubscriptionErrors.Inc()
			return err

		case txHash := <-hashChan:
			// Fetch full transaction by hash
			tx, _, err := client.TransactionByHash(ctx, txHash)
			if err != nil {
				continue
			}
			m.handleTransaction(tx)
		}
	}
}

func (m *Monitor) handleTransaction(tx *types.Transaction) {
	// Apply filters
	if m.config.FilterEnabled {
		// Check minimum value
		if tx.Value().Uint64() < uint64(m.config.MinValue) && len(tx.Data()) < 4 {
			return
		}

		// Check selector
		if len(tx.Data()) >= 4 {
			selector := "0x" + common.Bytes2Hex(tx.Data()[:4])
			if !m.selectors[selector] {
				return
			}
		}
	}

	// Convert to our format
	pendingTx := &PendingTx{
		Hash:      tx.Hash(),
		To:        tx.To(),
		Value:     tx.Value().Uint64(),
		GasPrice:  tx.GasPrice().Uint64(),
		GasLimit:  tx.Gas(),
		Input:     tx.Data(),
		Nonce:     tx.Nonce(),
		Timestamp: time.Now(),
	}

	// Get sender address
	signer := types.LatestSignerForChainID(tx.ChainId())
	if from, err := types.Sender(signer, tx); err == nil {
		pendingTx.From = from
	}

	metrics.MempoolTxReceived.Inc()
	metrics.MempoolTxFiltered.Inc()

	// Send to channel (non-blocking)
	select {
	case m.txChan <- pendingTx:
	default:
		metrics.MempoolTxDropped.Inc()
		log.Warn().Msg("Tx channel full, dropping transaction")
	}
}

func (m *Monitor) processLoop(ctx context.Context) {
	defer m.wg.Done()

	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()

	var count uint64

	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			if count > 0 {
				log.Info().Uint64("txs", count).Msg("Transactions processed")
				count = 0
			}

		case tx := <-m.txChan:
			count++
			// Process transaction - send to Rust core via FFI or channel
			m.forwardToCore(tx)
		}
	}
}

func (m *Monitor) forwardToCore(tx *PendingTx) {
	// TODO: Send to Rust core via FFI or gRPC
	// For now, just log
	log.Debug().
		Str("hash", tx.Hash.Hex()).
		Str("to", tx.To.Hex()).
		Uint64("value", tx.Value).
		Int("data_len", len(tx.Input)).
		Msg("Forwarding tx to core")
}
