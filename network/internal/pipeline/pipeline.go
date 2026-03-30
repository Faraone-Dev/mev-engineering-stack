package pipeline

import (
	"context"
	"encoding/hex"
	"sync"
	"sync/atomic"
	"time"

	"github.com/mev-protocol/network/internal/mempool"
	"github.com/mev-protocol/network/internal/metrics"
	"github.com/rs/zerolog/log"
)

// TxClass represents the classification of a pending transaction
type TxClass int

const (
	ClassUnknown     TxClass = iota
	ClassSwapV2              // UniswapV2-style swapExactTokensForTokens
	ClassSwapV3              // UniswapV3-style exactInputSingle
	ClassTransfer            // ERC20 transfer
	ClassApproval            // ERC20 approve
	ClassLiquidation         // Aave/Compound liquidation
	ClassFlashLoan           // Flash loan initiation
)

var classLabels = map[TxClass]string{
	ClassUnknown:     "unknown",
	ClassSwapV2:      "swap_v2",
	ClassSwapV3:      "swap_v3",
	ClassTransfer:    "transfer",
	ClassApproval:    "approval",
	ClassLiquidation: "liquidation",
	ClassFlashLoan:   "flash_loan",
}

// Known function selectors (first 4 bytes of keccak256)
var knownSelectors = map[string]TxClass{
	"38ed1739": ClassSwapV2,      // swapExactTokensForTokens
	"8803dbee": ClassSwapV2,      // swapTokensForExactTokens
	"7ff36ab5": ClassSwapV2,      // swapExactETHForTokens
	"18cbafe5": ClassSwapV2,      // swapExactTokensForETH
	"fb3bdb41": ClassSwapV2,      // swapETHForExactTokens
	"5c11d795": ClassSwapV2,      // swapExactTokensForTokensSupportingFeeOnTransferTokens
	"414bf389": ClassSwapV3,      // exactInputSingle
	"c04b8d59": ClassSwapV3,      // exactInput
	"db3e2198": ClassSwapV3,      // exactOutputSingle
	"f28c0498": ClassSwapV3,      // exactOutput
	"a9059cbb": ClassTransfer,    // transfer
	"23b872dd": ClassTransfer,    // transferFrom
	"095ea7b3": ClassApproval,    // approve
	"e8eda9df": ClassLiquidation, // liquidationCall (Aave V2)
	"00a718a9": ClassLiquidation, // liquidationCall (Aave V3)
	"ab62770f": ClassFlashLoan,   // flashLoan (Balancer)
	"5cffe9de": ClassFlashLoan,   // flashLoan (Aave)
}

// ClassifiedTx is a transaction with its classification
type ClassifiedTx struct {
	Tx    *mempool.PendingTx
	Class TxClass
	// Decoded swap parameters (populated for swap transactions)
	SwapInfo *SwapInfo
}

// SwapInfo contains decoded swap parameters
type SwapInfo struct {
	AmountIn  []byte // raw big.Int bytes
	AmountOut []byte // raw big.Int bytes — minimum output
	PathLen   int    // number of tokens in swap path
	Deadline  uint64
}

// Config for the pipeline
type Config struct {
	Workers         int
	ClassifyTimeout time.Duration
	BufferSize      int
}

// Pipeline processes transactions through classification and filtering stages
type Pipeline struct {
	config Config

	inputChan  <-chan *mempool.PendingTx
	outputChan chan *ClassifiedTx

	processed atomic.Uint64
	filtered  atomic.Uint64

	mu      sync.RWMutex
	running bool
	wg      sync.WaitGroup
}

// NewPipeline creates a new transaction processing pipeline
func NewPipeline(cfg Config, input <-chan *mempool.PendingTx) *Pipeline {
	return &Pipeline{
		config:     cfg,
		inputChan:  input,
		outputChan: make(chan *ClassifiedTx, cfg.BufferSize),
	}
}

// Start launches pipeline worker goroutines
func (p *Pipeline) Start(ctx context.Context) error {
	p.mu.Lock()
	p.running = true
	p.mu.Unlock()

	log.Info().
		Int("workers", p.config.Workers).
		Msg("Starting tx processing pipeline")

	for i := 0; i < p.config.Workers; i++ {
		p.wg.Add(1)
		go p.worker(ctx, i)
	}

	// Stats reporter
	p.wg.Add(1)
	go p.statsReporter(ctx)

	return nil
}

// Stop shuts down the pipeline
func (p *Pipeline) Stop(ctx context.Context) {
	p.mu.Lock()
	p.running = false
	p.mu.Unlock()

	log.Info().Msg("Stopping pipeline")
	p.wg.Wait()
	close(p.outputChan)
}

// OutputChan returns classified transactions
func (p *Pipeline) OutputChan() <-chan *ClassifiedTx {
	return p.outputChan
}

func (p *Pipeline) worker(ctx context.Context, id int) {
	defer p.wg.Done()

	log.Debug().Int("worker", id).Msg("Pipeline worker started")

	for {
		select {
		case <-ctx.Done():
			return

		case tx, ok := <-p.inputChan:
			if !ok {
				return
			}
			p.processTx(tx)
		}
	}
}

func (p *Pipeline) processTx(tx *mempool.PendingTx) {
	start := time.Now()

	// Stage 1: Classify
	class := classifyTx(tx)

	metrics.PipelineTxProcessed.WithLabelValues("classify").Inc()
	metrics.PipelineStageLatency.WithLabelValues("classify").Observe(time.Since(start).Seconds())

	p.processed.Add(1)

	// Stage 2: Filter — only forward interesting transactions
	if class == ClassUnknown || class == ClassApproval {
		return
	}

	p.filtered.Add(1)

	// Stage 3: Decode swap params if applicable
	var swapInfo *SwapInfo
	if class == ClassSwapV2 || class == ClassSwapV3 {
		swapInfo = decodeSwapInfo(tx.Input, class)
	}

	classified := &ClassifiedTx{
		Tx:       tx,
		Class:    class,
		SwapInfo: swapInfo,
	}

	label := classLabels[class]
	metrics.PipelineOpportunitiesFound.WithLabelValues(label).Inc()

	// Emit to output (non-blocking)
	select {
	case p.outputChan <- classified:
	default:
		log.Warn().
			Str("hash", tx.Hash.Hex()).
			Str("class", label).
			Msg("Pipeline output full, dropping classified tx")
	}
}

// classifyTx determines the transaction type from its calldata selector
func classifyTx(tx *mempool.PendingTx) TxClass {
	if len(tx.Input) < 4 {
		return ClassUnknown
	}

	selector := hex.EncodeToString(tx.Input[:4])

	if class, exists := knownSelectors[selector]; exists {
		return class
	}

	return ClassUnknown
}

// decodeSwapInfo extracts basic swap parameters from calldata
func decodeSwapInfo(data []byte, class TxClass) *SwapInfo {
	// ABI-encoded calldata: 4 bytes selector + 32-byte aligned params
	if len(data) < 68 { // 4 + 2*32 minimum
		return nil
	}

	info := &SwapInfo{}

	switch class {
	case ClassSwapV2:
		// swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
		if len(data) >= 164 { // 4 + 5*32
			info.AmountIn = data[4:36]
			info.AmountOut = data[36:68]
			// Path is dynamic — decode offset and length
			if len(data) >= 196 {
				// The 3rd param is offset to path array
				// Path length is at the offset position
				pathOffset := 128 + 4 // typical offset for 4th param
				if len(data) > pathOffset+32 {
					pathLenBytes := data[pathOffset : pathOffset+32]
					// Simple: count non-zero trailing byte as path length
					info.PathLen = int(pathLenBytes[31])
				}
			}
		}

	case ClassSwapV3:
		// exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))
		if len(data) >= 260 { // 4 + 8*32
			info.AmountIn = data[132:164]  // 5th param
			info.AmountOut = data[164:196] // 6th param
			info.PathLen = 2               // single-hop = 2 tokens
		}
	}

	return info
}

func (p *Pipeline) statsReporter(ctx context.Context) {
	defer p.wg.Done()

	ticker := time.NewTicker(10 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			processed := p.processed.Load()
			filtered := p.filtered.Load()

			if processed > 0 {
				log.Info().
					Uint64("processed", processed).
					Uint64("classified", filtered).
					Float64("hitRate", float64(filtered)/float64(processed)*100).
					Msg("Pipeline stats")
			}
		}
	}
}
