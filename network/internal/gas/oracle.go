package gas

import (
	"context"
	"math/big"
	"sync"
	"time"

	"github.com/mev-protocol/network/internal/block"
	"github.com/mev-protocol/network/internal/metrics"
	"github.com/mev-protocol/network/internal/rpc"
	"github.com/rs/zerolog/log"
)

var (
	bigGwei = big.NewInt(1_000_000_000)
)

// Config for gas oracle
type Config struct {
	HistorySize    int
	UpdateInterval time.Duration
	// EIP-1559 base fee elasticity multiplier (mainnet = 2)
	ElasticityMultiplier uint64
	// EIP-1559 base fee change denominator (mainnet = 8)
	BaseFeeChangeDenominator uint64
}

// Estimate represents a gas price estimate
type Estimate struct {
	BaseFee           *big.Int
	PredictedBaseFee  *big.Int
	SuggestedPriority *big.Int
	MaxFeePerGas      *big.Int
	LastUpdate        time.Time
}

// Oracle tracks and predicts gas prices using EIP-1559 mechanics
type Oracle struct {
	config  Config
	rpcPool *rpc.Pool
	watcher *block.Watcher

	history   []*blockGasInfo
	historyMu sync.RWMutex

	estimate   *Estimate
	estimateMu sync.RWMutex

	running bool
	mu      sync.RWMutex
	wg      sync.WaitGroup
}

type blockGasInfo struct {
	Number   uint64
	BaseFee  *big.Int
	GasUsed  uint64
	GasLimit uint64
	Time     uint64
}

// NewOracle creates a new gas oracle
func NewOracle(cfg Config, pool *rpc.Pool, watcher *block.Watcher) *Oracle {
	if cfg.ElasticityMultiplier == 0 {
		cfg.ElasticityMultiplier = 2
	}
	if cfg.BaseFeeChangeDenominator == 0 {
		cfg.BaseFeeChangeDenominator = 8
	}

	return &Oracle{
		config:  cfg,
		rpcPool: pool,
		watcher: watcher,
		history: make([]*blockGasInfo, 0, cfg.HistorySize),
	}
}

// Start begins tracking gas prices
func (o *Oracle) Start(ctx context.Context) error {
	o.mu.Lock()
	o.running = true
	o.mu.Unlock()

	log.Info().
		Int("historySize", o.config.HistorySize).
		Msg("Starting gas oracle")

	o.wg.Add(1)
	go o.trackLoop(ctx)

	return nil
}

// Stop shuts down the oracle
func (o *Oracle) Stop(ctx context.Context) {
	o.mu.Lock()
	o.running = false
	o.mu.Unlock()

	log.Info().Msg("Stopping gas oracle")
	o.wg.Wait()
}

// GetEstimate returns the current gas estimate
func (o *Oracle) GetEstimate() *Estimate {
	o.estimateMu.RLock()
	defer o.estimateMu.RUnlock()
	return o.estimate
}

// PredictBaseFee predicts base fee N blocks ahead
func (o *Oracle) PredictBaseFee(blocksAhead int) *big.Int {
	o.historyMu.RLock()
	defer o.historyMu.RUnlock()

	if len(o.history) == 0 {
		return nil
	}

	latest := o.history[len(o.history)-1]
	predicted := new(big.Int).Set(latest.BaseFee)

	// Simulate EIP-1559 base fee adjustment for each future block
	// assuming current gas usage trend continues
	avgGasRatio := o.averageGasRatio()

	for i := 0; i < blocksAhead; i++ {
		predicted = o.calculateNextBaseFee(predicted, avgGasRatio, latest.GasLimit)
	}

	return predicted
}

func (o *Oracle) trackLoop(ctx context.Context) {
	defer o.wg.Done()

	headerChan := o.watcher.HeaderChan()

	for {
		select {
		case <-ctx.Done():
			return

		case header := <-headerChan:
			o.processBlock(header)
		}
	}
}

func (o *Oracle) processBlock(header *block.Header) {
	if header.BaseFee == nil {
		return
	}

	info := &blockGasInfo{
		Number:   header.Number,
		BaseFee:  new(big.Int).Set(header.BaseFee),
		GasUsed:  header.GasUsed,
		GasLimit: header.GasLimit,
		Time:     header.Timestamp,
	}

	// Update history (ring buffer)
	o.historyMu.Lock()
	o.history = append(o.history, info)
	if len(o.history) > o.config.HistorySize {
		o.history = o.history[1:]
	}
	o.historyMu.Unlock()

	// Recalculate estimate
	o.updateEstimate(info)
}

func (o *Oracle) updateEstimate(latest *blockGasInfo) {
	predicted := o.PredictBaseFee(1)
	if predicted == nil {
		predicted = latest.BaseFee
	}

	// Calculate suggested priority fee from recent history
	suggestedPriority := o.calculatePriorityFee()

	// Max fee = 2 * predicted base fee + priority fee
	// This gives headroom for 1 additional block of max base fee increase
	maxFee := new(big.Int).Mul(predicted, big.NewInt(2))
	maxFee.Add(maxFee, suggestedPriority)

	estimate := &Estimate{
		BaseFee:           new(big.Int).Set(latest.BaseFee),
		PredictedBaseFee:  predicted,
		SuggestedPriority: suggestedPriority,
		MaxFeePerGas:      maxFee,
		LastUpdate:        time.Now(),
	}

	o.estimateMu.Lock()
	o.estimate = estimate
	o.estimateMu.Unlock()

	// Update Prometheus metrics
	baseFeeGwei, _ := new(big.Float).Quo(
		new(big.Float).SetInt(latest.BaseFee),
		new(big.Float).SetInt(bigGwei),
	).Float64()
	metrics.GasBaseFee.Set(baseFeeGwei)

	predictedGwei, _ := new(big.Float).Quo(
		new(big.Float).SetInt(predicted),
		new(big.Float).SetInt(bigGwei),
	).Float64()
	metrics.GasPredictedBaseFee.Set(predictedGwei)

	priorityGwei, _ := new(big.Float).Quo(
		new(big.Float).SetInt(suggestedPriority),
		new(big.Float).SetInt(bigGwei),
	).Float64()
	metrics.GasPriorityFee.Set(priorityGwei)

	log.Debug().
		Float64("baseFee_gwei", baseFeeGwei).
		Float64("predicted_gwei", predictedGwei).
		Float64("priority_gwei", priorityGwei).
		Msg("Gas estimate updated")
}

// calculateNextBaseFee implements the EIP-1559 base fee formula
func (o *Oracle) calculateNextBaseFee(currentBaseFee *big.Int, gasRatio float64, gasLimit uint64) *big.Int {
	gasTarget := gasLimit / o.config.ElasticityMultiplier
	denominator := new(big.Int).SetUint64(o.config.BaseFeeChangeDenominator)

	// Simulate gas used based on average ratio
	gasUsed := uint64(gasRatio * float64(gasLimit))

	next := new(big.Int)

	if gasUsed == gasTarget {
		// Base fee unchanged
		next.Set(currentBaseFee)
	} else if gasUsed > gasTarget {
		// Base fee increases
		// newBaseFee = currentBaseFee + currentBaseFee * (gasUsed - gasTarget) / gasTarget / denominator
		delta := new(big.Int).SetUint64(gasUsed - gasTarget)
		delta.Mul(currentBaseFee, delta)
		delta.Div(delta, new(big.Int).SetUint64(gasTarget))
		delta.Div(delta, denominator)

		// Minimum increase of 1 wei
		if delta.Sign() == 0 {
			delta.SetInt64(1)
		}

		next.Add(currentBaseFee, delta)
	} else {
		// Base fee decreases
		// newBaseFee = currentBaseFee - currentBaseFee * (gasTarget - gasUsed) / gasTarget / denominator
		delta := new(big.Int).SetUint64(gasTarget - gasUsed)
		delta.Mul(currentBaseFee, delta)
		delta.Div(delta, new(big.Int).SetUint64(gasTarget))
		delta.Div(delta, denominator)

		next.Sub(currentBaseFee, delta)

		// Floor at 0
		if next.Sign() < 0 {
			next.SetInt64(0)
		}
	}

	return next
}

func (o *Oracle) averageGasRatio() float64 {
	o.historyMu.RLock()
	defer o.historyMu.RUnlock()

	if len(o.history) == 0 {
		return 0.5 // Assume 50% utilization
	}

	var total float64
	for _, h := range o.history {
		if h.GasLimit > 0 {
			total += float64(h.GasUsed) / float64(h.GasLimit)
		}
	}

	return total / float64(len(o.history))
}

func (o *Oracle) calculatePriorityFee() *big.Int {
	// Conservative: 2 Gwei default priority fee
	// In production, this would sample recent blocks' effective priority fees
	// via eth_feeHistory RPC call
	return new(big.Int).Mul(big.NewInt(2), bigGwei)
}
