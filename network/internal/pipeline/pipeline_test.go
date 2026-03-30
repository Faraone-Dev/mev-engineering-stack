package pipeline

import (
	"context"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/mev-protocol/network/internal/mempool"
)

func TestClassifyTx_SwapV2(t *testing.T) {
	tests := []struct {
		name     string
		selector []byte
		want     TxClass
	}{
		{
			name:     "swapExactTokensForTokens",
			selector: []byte{0x38, 0xed, 0x17, 0x39},
			want:     ClassSwapV2,
		},
		{
			name:     "swapTokensForExactTokens",
			selector: []byte{0x88, 0x03, 0xdb, 0xee},
			want:     ClassSwapV2,
		},
		{
			name:     "swapExactETHForTokens",
			selector: []byte{0x7f, 0xf3, 0x6a, 0xb5},
			want:     ClassSwapV2,
		},
		{
			name:     "swapExactTokensForETH",
			selector: []byte{0x18, 0xcb, 0xaf, 0xe5},
			want:     ClassSwapV2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tx := &mempool.PendingTx{
				Input: append(tt.selector, make([]byte, 256)...),
			}
			got := classifyTx(tx)
			if got != tt.want {
				t.Errorf("classifyTx() = %d, want %d", got, tt.want)
			}
		})
	}
}

func TestClassifyTx_SwapV3(t *testing.T) {
	tests := []struct {
		name     string
		selector []byte
		want     TxClass
	}{
		{
			name:     "exactInputSingle",
			selector: []byte{0x41, 0x4b, 0xf3, 0x89},
			want:     ClassSwapV3,
		},
		{
			name:     "exactInput",
			selector: []byte{0xc0, 0x4b, 0x8d, 0x59},
			want:     ClassSwapV3,
		},
		{
			name:     "exactOutputSingle",
			selector: []byte{0xdb, 0x3e, 0x21, 0x98},
			want:     ClassSwapV3,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tx := &mempool.PendingTx{
				Input: append(tt.selector, make([]byte, 256)...),
			}
			got := classifyTx(tx)
			if got != tt.want {
				t.Errorf("classifyTx() = %d, want %d", got, tt.want)
			}
		})
	}
}

func TestClassifyTx_Transfers(t *testing.T) {
	// ERC20 transfer
	tx := &mempool.PendingTx{
		Input: append([]byte{0xa9, 0x05, 0x9c, 0xbb}, make([]byte, 64)...),
	}
	if got := classifyTx(tx); got != ClassTransfer {
		t.Errorf("transfer: got %d, want %d", got, ClassTransfer)
	}

	// ERC20 approve
	tx2 := &mempool.PendingTx{
		Input: append([]byte{0x09, 0x5e, 0xa7, 0xb3}, make([]byte, 64)...),
	}
	if got := classifyTx(tx2); got != ClassApproval {
		t.Errorf("approve: got %d, want %d", got, ClassApproval)
	}
}

func TestClassifyTx_Liquidation(t *testing.T) {
	tx := &mempool.PendingTx{
		Input: append([]byte{0xe8, 0xed, 0xa9, 0xdf}, make([]byte, 128)...),
	}
	if got := classifyTx(tx); got != ClassLiquidation {
		t.Errorf("liquidation: got %d, want %d", got, ClassLiquidation)
	}
}

func TestClassifyTx_Unknown(t *testing.T) {
	// Empty calldata
	tx1 := &mempool.PendingTx{Input: nil}
	if got := classifyTx(tx1); got != ClassUnknown {
		t.Errorf("empty: got %d, want %d", got, ClassUnknown)
	}

	// Short calldata
	tx2 := &mempool.PendingTx{Input: []byte{0x01, 0x02}}
	if got := classifyTx(tx2); got != ClassUnknown {
		t.Errorf("short: got %d, want %d", got, ClassUnknown)
	}

	// Unknown selector
	tx3 := &mempool.PendingTx{Input: []byte{0xde, 0xad, 0xbe, 0xef}}
	if got := classifyTx(tx3); got != ClassUnknown {
		t.Errorf("unknown: got %d, want %d", got, ClassUnknown)
	}
}

func TestDecodeSwapInfo_V2(t *testing.T) {
	// Build a minimal swapExactTokensForTokens calldata
	// selector (4) + amountIn (32) + amountOutMin (32) + offset (32) + to (32) + deadline (32) = 164
	data := make([]byte, 200)
	copy(data[0:4], []byte{0x38, 0xed, 0x17, 0x39})
	data[35] = 0x01 // amountIn = 1
	data[67] = 0x02 // amountOutMin = 2

	info := decodeSwapInfo(data, ClassSwapV2)
	if info == nil {
		t.Fatal("expected non-nil SwapInfo")
	}
	if info.AmountIn == nil {
		t.Fatal("expected AmountIn")
	}
}

func TestDecodeSwapInfo_TooShort(t *testing.T) {
	data := make([]byte, 36) // Too short for swap
	info := decodeSwapInfo(data, ClassSwapV2)
	if info != nil {
		t.Error("expected nil for short data")
	}
}

func TestPipeline_StartStop(t *testing.T) {
	inputChan := make(chan *mempool.PendingTx, 100)
	p := NewPipeline(Config{
		Workers:         2,
		ClassifyTimeout: 10 * time.Millisecond,
		BufferSize:      100,
	}, inputChan)

	ctx, cancel := context.WithCancel(context.Background())
	if err := p.Start(ctx); err != nil {
		t.Fatal(err)
	}

	// Send some test transactions
	for i := 0; i < 10; i++ {
		inputChan <- &mempool.PendingTx{
			Hash:  common.HexToHash("0xdeadbeef"),
			Input: append([]byte{0x38, 0xed, 0x17, 0x39}, make([]byte, 256)...),
		}
	}

	// Give workers time to process
	time.Sleep(50 * time.Millisecond)

	// Should have classified transactions in output
	select {
	case tx := <-p.OutputChan():
		if tx.Class != ClassSwapV2 {
			t.Errorf("expected ClassSwapV2, got %d", tx.Class)
		}
	case <-time.After(time.Second):
		t.Error("timeout waiting for classified tx")
	}

	cancel()
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), time.Second)
	defer shutdownCancel()
	p.Stop(shutdownCtx)
}

func TestPipeline_FiltersApprovals(t *testing.T) {
	inputChan := make(chan *mempool.PendingTx, 100)
	p := NewPipeline(Config{
		Workers:    1,
		BufferSize: 100,
	}, inputChan)

	ctx, cancel := context.WithCancel(context.Background())
	if err := p.Start(ctx); err != nil {
		t.Fatal(err)
	}

	// Send an approval (should be filtered out)
	inputChan <- &mempool.PendingTx{
		Hash:  common.HexToHash("0x01"),
		Input: append([]byte{0x09, 0x5e, 0xa7, 0xb3}, make([]byte, 64)...),
	}

	// Send a swap (should pass through)
	inputChan <- &mempool.PendingTx{
		Hash:  common.HexToHash("0x02"),
		Input: append([]byte{0x38, 0xed, 0x17, 0x39}, make([]byte, 256)...),
	}

	time.Sleep(50 * time.Millisecond)

	// Only the swap should come through
	select {
	case tx := <-p.OutputChan():
		if tx.Class != ClassSwapV2 {
			t.Errorf("expected swap, got class %d", tx.Class)
		}
	case <-time.After(time.Second):
		t.Error("timeout — expected swap tx")
	}

	cancel()
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), time.Second)
	defer shutdownCancel()
	p.Stop(shutdownCtx)
}

func BenchmarkClassifyTx(b *testing.B) {
	tx := &mempool.PendingTx{
		Input: append([]byte{0x38, 0xed, 0x17, 0x39}, make([]byte, 256)...),
	}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		classifyTx(tx)
	}
}
