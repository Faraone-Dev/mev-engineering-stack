package gas

import (
	"math/big"
	"testing"
)

func TestCalculateNextBaseFee_AtTarget(t *testing.T) {
	o := &Oracle{
		config: Config{
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
	}

	baseFee := big.NewInt(30_000_000_000) // 30 Gwei
	gasLimit := uint64(30_000_000)
	gasTarget := gasLimit / 2                          // 15M
	gasRatio := float64(gasTarget) / float64(gasLimit) // Exactly at target

	next := o.calculateNextBaseFee(baseFee, gasRatio, gasLimit)

	if next.Cmp(baseFee) != 0 {
		t.Errorf("at target: expected %s, got %s", baseFee, next)
	}
}

func TestCalculateNextBaseFee_FullBlocks(t *testing.T) {
	o := &Oracle{
		config: Config{
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
	}

	baseFee := big.NewInt(30_000_000_000) // 30 Gwei
	gasLimit := uint64(30_000_000)

	// 100% full block — max increase
	next := o.calculateNextBaseFee(baseFee, 1.0, gasLimit)

	// Expected: baseFee + baseFee * (30M - 15M) / 15M / 8 = baseFee + baseFee * 1/8
	// = 30 + 3.75 = 33.75 Gwei
	expected := new(big.Int).Add(baseFee, new(big.Int).Div(baseFee, big.NewInt(8)))

	if next.Cmp(expected) != 0 {
		t.Errorf("full block: expected %s, got %s", expected, next)
	}
}

func TestCalculateNextBaseFee_EmptyBlocks(t *testing.T) {
	o := &Oracle{
		config: Config{
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
	}

	baseFee := big.NewInt(30_000_000_000) // 30 Gwei
	gasLimit := uint64(30_000_000)

	// 0% utilization — max decrease
	next := o.calculateNextBaseFee(baseFee, 0.0, gasLimit)

	// Expected: baseFee - baseFee * 15M / 15M / 8 = baseFee - baseFee/8
	// = 30 - 3.75 = 26.25 Gwei
	expected := new(big.Int).Sub(baseFee, new(big.Int).Div(baseFee, big.NewInt(8)))

	if next.Cmp(expected) != 0 {
		t.Errorf("empty block: expected %s, got %s", expected, next)
	}
}

func TestCalculateNextBaseFee_FloorAtZero(t *testing.T) {
	o := &Oracle{
		config: Config{
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
	}

	// Very low base fee — should not go negative
	baseFee := big.NewInt(1)
	gasLimit := uint64(30_000_000)

	next := o.calculateNextBaseFee(baseFee, 0.0, gasLimit)

	if next.Sign() < 0 {
		t.Errorf("base fee went negative: %s", next)
	}
}

func TestPredictBaseFee_MultiBlock(t *testing.T) {
	o := &Oracle{
		config: Config{
			HistorySize:              10,
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
		history: make([]*blockGasInfo, 0),
	}

	// Simulate 5 blocks with 60% utilization
	baseFee := big.NewInt(20_000_000_000) // 20 Gwei
	for i := 0; i < 5; i++ {
		o.history = append(o.history, &blockGasInfo{
			Number:   uint64(100 + i),
			BaseFee:  new(big.Int).Set(baseFee),
			GasUsed:  18_000_000, // 60% of 30M
			GasLimit: 30_000_000,
		})
	}

	// Predict 1 block ahead
	predicted := o.PredictBaseFee(1)
	if predicted == nil {
		t.Fatal("expected prediction, got nil")
	}

	// With 60% utilization (above 50% target), base fee should increase
	if predicted.Cmp(baseFee) <= 0 {
		t.Errorf("60%% utilization should increase base fee: current=%s, predicted=%s", baseFee, predicted)
	}

	// Predict 3 blocks: should be higher than 1-block prediction
	predicted3 := o.PredictBaseFee(3)
	if predicted3.Cmp(predicted) <= 0 {
		t.Errorf("3-block prediction should be higher than 1-block: 1=%s, 3=%s", predicted, predicted3)
	}
}

func TestPredictBaseFee_EmptyHistory(t *testing.T) {
	o := &Oracle{
		config:  Config{HistorySize: 10},
		history: make([]*blockGasInfo, 0),
	}

	result := o.PredictBaseFee(1)
	if result != nil {
		t.Errorf("expected nil for empty history, got %s", result)
	}
}

func TestAverageGasRatio(t *testing.T) {
	o := &Oracle{
		config:  Config{HistorySize: 10},
		history: make([]*blockGasInfo, 0),
	}

	// Empty history
	ratio := o.averageGasRatio()
	if ratio != 0.5 {
		t.Errorf("empty: expected 0.5, got %f", ratio)
	}

	// Add blocks
	o.history = append(o.history,
		&blockGasInfo{GasUsed: 15_000_000, GasLimit: 30_000_000}, // 50%
		&blockGasInfo{GasUsed: 24_000_000, GasLimit: 30_000_000}, // 80%
		&blockGasInfo{GasUsed: 6_000_000, GasLimit: 30_000_000},  // 20%
	)

	ratio = o.averageGasRatio()
	expected := 0.5 // (0.5 + 0.8 + 0.2) / 3 = 0.5
	if ratio < expected-0.01 || ratio > expected+0.01 {
		t.Errorf("expected ~%f, got %f", expected, ratio)
	}
}

func BenchmarkCalculateNextBaseFee(b *testing.B) {
	o := &Oracle{
		config: Config{
			ElasticityMultiplier:     2,
			BaseFeeChangeDenominator: 8,
		},
	}

	baseFee := big.NewInt(30_000_000_000)
	gasLimit := uint64(30_000_000)

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		o.calculateNextBaseFee(baseFee, 0.65, gasLimit)
	}
}
