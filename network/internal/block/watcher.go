package block

import (
	"context"
	"math/big"
	"sync"
	"time"

	ethtypes "github.com/ethereum/go-ethereum/core/types"
	"github.com/mev-protocol/network/internal/metrics"
	"github.com/mev-protocol/network/internal/rpc"
	"github.com/rs/zerolog/log"
)

// Config for block watcher
type Config struct {
	BufferSize    int
	PollInterval  time.Duration
	MaxReorgDepth int
	TrackBaseFee  bool
}

// Header contains processed block header information
type Header struct {
	Number     uint64
	Hash       [32]byte
	ParentHash [32]byte
	Timestamp  uint64
	BaseFee    *big.Int
	GasUsed    uint64
	GasLimit   uint64
	Difficulty *big.Int
	ObservedAt time.Time
}

// Watcher monitors new block headers
type Watcher struct {
	config  Config
	rpcPool *rpc.Pool

	headerChan chan *Header
	latest     *Header

	mu      sync.RWMutex
	running bool
	wg      sync.WaitGroup
}

// NewWatcher creates a new block watcher
func NewWatcher(cfg Config, pool *rpc.Pool) *Watcher {
	return &Watcher{
		config:     cfg,
		rpcPool:    pool,
		headerChan: make(chan *Header, cfg.BufferSize),
	}
}

// Start begins watching for new blocks
func (w *Watcher) Start(ctx context.Context) error {
	w.mu.Lock()
	w.running = true
	w.mu.Unlock()

	log.Info().
		Int("buffer", w.config.BufferSize).
		Int("maxReorgDepth", w.config.MaxReorgDepth).
		Msg("Starting block watcher")

	w.wg.Add(1)
	go w.subscribeLoop(ctx)

	return nil
}

// Stop gracefully stops the watcher
func (w *Watcher) Stop(ctx context.Context) {
	w.mu.Lock()
	w.running = false
	w.mu.Unlock()

	log.Info().Msg("Stopping block watcher")
	w.wg.Wait()
}

// HeaderChan returns the channel for new block headers
func (w *Watcher) HeaderChan() <-chan *Header {
	return w.headerChan
}

// LatestBlock returns the latest observed block header
func (w *Watcher) LatestBlock() *Header {
	w.mu.RLock()
	defer w.mu.RUnlock()
	return w.latest
}

// TargetBlock returns the next block number for bundle targeting
func (w *Watcher) TargetBlock() uint64 {
	w.mu.RLock()
	defer w.mu.RUnlock()

	if w.latest == nil {
		return 0
	}
	return w.latest.Number + 1
}

func (w *Watcher) subscribeLoop(ctx context.Context) {
	defer w.wg.Done()

	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		w.mu.RLock()
		if !w.running {
			w.mu.RUnlock()
			return
		}
		w.mu.RUnlock()

		if err := w.subscribe(ctx); err != nil {
			log.Error().Err(err).Msg("Block subscription error, reconnecting...")
			time.Sleep(time.Second)
		}
	}
}

func (w *Watcher) subscribe(ctx context.Context) error {
	client, err := w.rpcPool.GetWSClient()
	if err != nil {
		return err
	}

	headerChan := make(chan *ethtypes.Header, 16)
	sub, err := client.SubscribeNewHead(ctx, headerChan)
	if err != nil {
		// Fallback to polling if subscription not supported
		log.Warn().Err(err).Msg("Header subscription failed, falling back to polling")
		return w.pollHeaders(ctx)
	}
	defer sub.Unsubscribe()

	log.Info().Msg("Subscribed to new block headers")

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()

		case err := <-sub.Err():
			return err

		case header := <-headerChan:
			w.handleHeader(header)
		}
	}
}

func (w *Watcher) pollHeaders(ctx context.Context) error {
	ticker := time.NewTicker(w.config.PollInterval)
	defer ticker.Stop()

	var lastBlock uint64

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()

		case <-ticker.C:
			client, err := w.rpcPool.GetClient()
			if err != nil {
				continue
			}

			blockNum, err := client.BlockNumber(ctx)
			if err != nil {
				metrics.RPCRequestsTotal.WithLabelValues("poll", "error").Inc()
				continue
			}

			if blockNum > lastBlock {
				header, err := client.HeaderByNumber(ctx, new(big.Int).SetUint64(blockNum))
				if err != nil {
					continue
				}
				w.handleHeader(header)
				lastBlock = blockNum
			}
		}
	}
}

func (w *Watcher) handleHeader(ethHeader *ethtypes.Header) {
	observedAt := time.Now()

	header := &Header{
		Number:     ethHeader.Number.Uint64(),
		Timestamp:  ethHeader.Time,
		BaseFee:    ethHeader.BaseFee,
		GasUsed:    ethHeader.GasUsed,
		GasLimit:   ethHeader.GasLimit,
		Difficulty: ethHeader.Difficulty,
		ObservedAt: observedAt,
	}
	copy(header.Hash[:], ethHeader.Hash().Bytes())
	copy(header.ParentHash[:], ethHeader.ParentHash.Bytes())

	// Detect reorgs
	w.mu.RLock()
	prev := w.latest
	w.mu.RUnlock()

	if prev != nil && header.Number <= prev.Number {
		log.Warn().
			Uint64("expected", prev.Number+1).
			Uint64("received", header.Number).
			Msg("Possible reorg detected")
	}

	// Update latest
	w.mu.Lock()
	w.latest = header
	w.mu.Unlock()

	// Record metrics
	metrics.BlockLatestNumber.Set(float64(header.Number))

	if header.BaseFee != nil {
		baseFeeGwei := new(big.Float).Quo(
			new(big.Float).SetInt(header.BaseFee),
			new(big.Float).SetFloat64(1e9),
		)
		gwei, _ := baseFeeGwei.Float64()
		metrics.BlockBaseFee.Set(gwei)
	}

	blockTime := time.Unix(int64(header.Timestamp), 0)
	propagationDelay := observedAt.Sub(blockTime).Seconds()
	metrics.BlockProcessingLatency.Observe(propagationDelay)

	log.Info().
		Uint64("number", header.Number).
		Uint64("gasUsed", header.GasUsed).
		Uint64("gasLimit", header.GasLimit).
		Float64("gasRatio", float64(header.GasUsed)/float64(header.GasLimit)).
		Dur("propagation", observedAt.Sub(blockTime)).
		Msg("New block")

	// Emit to channel (non-blocking)
	select {
	case w.headerChan <- header:
	default:
		log.Warn().Uint64("block", header.Number).Msg("Header channel full")
	}
}
