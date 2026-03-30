package config

import (
	"os"
	"testing"
)

func TestLoad_Defaults(t *testing.T) {
	// Clear any env vars that might interfere
	os.Unsetenv("MEV_RPC_ENDPOINTS")
	os.Unsetenv("FLASHBOTS_SIGNING_KEY")

	cfg, err := Load()
	if err != nil {
		t.Fatal(err)
	}

	if len(cfg.RPC.Endpoints) == 0 {
		t.Error("expected default endpoints")
	}

	if cfg.Mempool.BufferSize != 10000 {
		t.Errorf("expected default buffer 10000, got %d", cfg.Mempool.BufferSize)
	}

	if !cfg.Mempool.FilterEnabled {
		t.Error("expected filter enabled by default")
	}

	if cfg.Pipeline.Workers != 4 {
		t.Errorf("expected 4 workers, got %d", cfg.Pipeline.Workers)
	}

	if cfg.Block.MaxReorgDepth != 64 {
		t.Errorf("expected max reorg 64, got %d", cfg.Block.MaxReorgDepth)
	}

	if cfg.Gas.ElasticityMultiplier != 2 {
		t.Errorf("expected elasticity 2, got %d", cfg.Gas.ElasticityMultiplier)
	}

	if !cfg.Metrics.Enabled {
		t.Error("expected metrics enabled by default")
	}

	if cfg.Metrics.Addr != ":9090" {
		t.Errorf("expected metrics addr :9090, got %s", cfg.Metrics.Addr)
	}
}

func TestLoad_EnvOverrides(t *testing.T) {
	os.Setenv("MEV_RPC_ENDPOINTS", "wss://node1.test,wss://node2.test")
	os.Setenv("MEV_MEMPOOL_BUFFER", "5000")
	os.Setenv("MEV_PIPELINE_WORKERS", "8")
	os.Setenv("MEV_METRICS_ENABLED", "false")
	os.Setenv("MEV_METRICS_ADDR", ":3000")
	defer func() {
		os.Unsetenv("MEV_RPC_ENDPOINTS")
		os.Unsetenv("MEV_MEMPOOL_BUFFER")
		os.Unsetenv("MEV_PIPELINE_WORKERS")
		os.Unsetenv("MEV_METRICS_ENABLED")
		os.Unsetenv("MEV_METRICS_ADDR")
	}()

	cfg, err := Load()
	if err != nil {
		t.Fatal(err)
	}

	if len(cfg.RPC.Endpoints) != 2 {
		t.Errorf("expected 2 endpoints, got %d", len(cfg.RPC.Endpoints))
	}

	if cfg.RPC.Endpoints[0] != "wss://node1.test" {
		t.Errorf("unexpected endpoint: %s", cfg.RPC.Endpoints[0])
	}

	if cfg.Mempool.BufferSize != 5000 {
		t.Errorf("expected buffer 5000, got %d", cfg.Mempool.BufferSize)
	}

	if cfg.Pipeline.Workers != 8 {
		t.Errorf("expected 8 workers, got %d", cfg.Pipeline.Workers)
	}

	if cfg.Metrics.Enabled {
		t.Error("expected metrics disabled")
	}

	if cfg.Metrics.Addr != ":3000" {
		t.Errorf("expected metrics addr :3000, got %s", cfg.Metrics.Addr)
	}
}

func TestEnvStringSlice(t *testing.T) {
	tests := []struct {
		name     string
		value    string
		fallback []string
		want     int
	}{
		{"empty", "", []string{"a", "b"}, 2},
		{"single", "x", nil, 1},
		{"multi", "a,b,c", nil, 3},
		{"spaces", " a , b , c ", nil, 3},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			key := "TEST_SLICE_" + tt.name
			if tt.value != "" {
				os.Setenv(key, tt.value)
				defer os.Unsetenv(key)
			}
			got := envStringSlice(key, tt.fallback)
			if len(got) != tt.want {
				t.Errorf("expected %d items, got %d: %v", tt.want, len(got), got)
			}
		})
	}
}

func TestEnvBool(t *testing.T) {
	os.Setenv("TEST_BOOL_TRUE", "true")
	os.Setenv("TEST_BOOL_FALSE", "false")
	os.Setenv("TEST_BOOL_INVALID", "maybe")
	defer func() {
		os.Unsetenv("TEST_BOOL_TRUE")
		os.Unsetenv("TEST_BOOL_FALSE")
		os.Unsetenv("TEST_BOOL_INVALID")
	}()

	if !envBool("TEST_BOOL_TRUE", false) {
		t.Error("expected true")
	}
	if envBool("TEST_BOOL_FALSE", true) {
		t.Error("expected false")
	}
	if !envBool("TEST_BOOL_INVALID", true) {
		t.Error("expected fallback true for invalid")
	}
	if envBool("TEST_BOOL_MISSING", false) {
		t.Error("expected fallback false for missing")
	}
}
