package relay

import (
	"context"
	"testing"
	"time"
)

// mockRelay implements the Relay interface for testing
type mockRelay struct {
	name      RelayType
	responses []*BundleResponse
	errors    []error
	calls     int
	delay     time.Duration
}

func (m *mockRelay) Name() RelayType { return m.name }

func (m *mockRelay) SendBundle(ctx context.Context, bundle *Bundle) (*BundleResponse, error) {
	if m.delay > 0 {
		select {
		case <-time.After(m.delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}

	idx := m.calls
	m.calls++

	if idx < len(m.errors) && m.errors[idx] != nil {
		return nil, m.errors[idx]
	}
	if idx < len(m.responses) {
		return m.responses[idx], nil
	}
	return &BundleResponse{BundleHash: "0xdefault"}, nil
}

func (m *mockRelay) SimulateBundle(ctx context.Context, bundle *Bundle) (*SimulationResult, error) {
	return &SimulationResult{BundleHash: "0xsim"}, nil
}

func TestManager_StrategyRace_FirstWins(t *testing.T) {
	fast := &mockRelay{
		name:      "fast",
		delay:     10 * time.Millisecond,
		responses: []*BundleResponse{{BundleHash: "0xfast"}},
	}
	slow := &mockRelay{
		name:      "slow",
		delay:     100 * time.Millisecond,
		responses: []*BundleResponse{{BundleHash: "0xslow"}},
	}

	mgr := NewManager(MultiConfig{
		Strategy:      StrategyRace,
		SubmitTimeout: 5 * time.Second,
	})
	mgr.AddRelay(fast, true)
	mgr.AddRelay(slow, false)

	results, err := mgr.SubmitBundle(context.Background(), &Bundle{
		Txs:         []string{"0xdead"},
		BlockNumber: "0x1",
	})

	if err != nil {
		t.Fatal(err)
	}

	// First result should be the winner
	found := false
	for _, r := range results {
		if r.Error == nil && r.Response.BundleHash == "0xfast" {
			found = true
			break
		}
	}

	if !found {
		t.Error("expected fast relay to win the race")
	}
}

func TestManager_StrategyPrimary_Fallback(t *testing.T) {
	primary := &mockRelay{
		name:   "primary",
		errors: []error{ErrAllRelaysFailed}, // Primary fails
	}
	backup := &mockRelay{
		name:      "backup",
		responses: []*BundleResponse{{BundleHash: "0xbackup"}},
	}

	mgr := NewManager(MultiConfig{
		Strategy:      StrategyPrimary,
		SubmitTimeout: 5 * time.Second,
	})
	mgr.AddRelay(primary, true)
	mgr.AddRelay(backup, false)

	results, err := mgr.SubmitBundle(context.Background(), &Bundle{
		Txs:         []string{"0xdead"},
		BlockNumber: "0x1",
	})

	if err != nil {
		t.Fatal(err)
	}

	// Should have 2 results (primary fail + backup success)
	if len(results) != 2 {
		t.Errorf("expected 2 results, got %d", len(results))
	}

	// Last result should be backup success
	last := results[len(results)-1]
	if last.Error != nil {
		t.Errorf("backup should have succeeded: %v", last.Error)
	}
	if last.Response.BundleHash != "0xbackup" {
		t.Errorf("expected backup hash, got %s", last.Response.BundleHash)
	}
}

func TestManager_StrategyAll(t *testing.T) {
	r1 := &mockRelay{
		name:      "relay1",
		responses: []*BundleResponse{{BundleHash: "0x1"}},
	}
	r2 := &mockRelay{
		name:      "relay2",
		responses: []*BundleResponse{{BundleHash: "0x2"}},
	}
	r3 := &mockRelay{
		name:   "relay3",
		errors: []error{ErrAllRelaysFailed},
	}

	mgr := NewManager(MultiConfig{
		Strategy:      StrategyAll,
		SubmitTimeout: 5 * time.Second,
	})
	mgr.AddRelay(r1, true)
	mgr.AddRelay(r2, false)
	mgr.AddRelay(r3, false)

	results, err := mgr.SubmitBundle(context.Background(), &Bundle{
		Txs:         []string{"0xdead"},
		BlockNumber: "0x1",
	})

	if err != nil {
		t.Fatal(err)
	}

	if len(results) != 3 {
		t.Errorf("expected 3 results, got %d", len(results))
	}

	successes := 0
	for _, r := range results {
		if r.Error == nil {
			successes++
		}
	}

	if successes != 2 {
		t.Errorf("expected 2 successes, got %d", successes)
	}
}

func TestManager_NoRelays(t *testing.T) {
	mgr := NewManager(MultiConfig{Strategy: StrategyRace})

	_, err := mgr.SubmitBundle(context.Background(), &Bundle{})
	if err != ErrNoRelays {
		t.Errorf("expected ErrNoRelays, got %v", err)
	}
}

func TestManager_Stats(t *testing.T) {
	relay := &mockRelay{
		name:      "test",
		responses: []*BundleResponse{{BundleHash: "0x1"}},
	}

	mgr := NewManager(MultiConfig{
		Strategy:      StrategyAll,
		SubmitTimeout: 5 * time.Second,
	})
	mgr.AddRelay(relay, true)

	mgr.SubmitBundle(context.Background(), &Bundle{Txs: []string{"0x1"}, BlockNumber: "0x1"})
	mgr.SubmitBundle(context.Background(), &Bundle{Txs: []string{"0x2"}, BlockNumber: "0x2"})

	submitted, succeeded, _ := mgr.Stats()
	if submitted != 2 {
		t.Errorf("expected 2 submitted, got %d", submitted)
	}
	if succeeded != 2 {
		t.Errorf("expected 2 succeeded, got %d", succeeded)
	}
}
