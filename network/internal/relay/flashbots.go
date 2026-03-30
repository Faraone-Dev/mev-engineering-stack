package relay

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/rs/zerolog/log"
)

// Config for relay client
type Config struct {
	FlashbotsURL  string
	BloXrouteURL  string
	SigningKey    string
	MaxRetries    int
	SubmitTimeout time.Duration
}

// Bundle represents a Flashbots bundle
type Bundle struct {
	Txs               []string `json:"txs"`
	BlockNumber       string   `json:"blockNumber"`
	MinTimestamp      *uint64  `json:"minTimestamp,omitempty"`
	MaxTimestamp      *uint64  `json:"maxTimestamp,omitempty"`
	RevertingTxHashes []string `json:"revertingTxHashes,omitempty"`
}

// BundleResponse from Flashbots
type BundleResponse struct {
	BundleHash string `json:"bundleHash"`
}

// Flashbots relay client
type Flashbots struct {
	config     Config
	httpClient *http.Client
	signingKey *ecdsa.PrivateKey
	running    bool
}

// NewFlashbots creates a new Flashbots relay client
func NewFlashbots(cfg Config) *Flashbots {
	return &Flashbots{
		config: cfg,
		httpClient: &http.Client{
			Timeout: cfg.SubmitTimeout,
		},
	}
}

// Name returns the relay type identifier (implements Relay interface)
func (f *Flashbots) Name() RelayType {
	return RelayFlashbots
}

// Start initializes the relay
func (f *Flashbots) Start(ctx context.Context) error {
	log.Info().Msg("Starting Flashbots relay")

	// Parse signing key
	if f.config.SigningKey != "" {
		key, err := crypto.HexToECDSA(f.config.SigningKey)
		if err != nil {
			return fmt.Errorf("invalid signing key: %w", err)
		}
		f.signingKey = key

		addr := crypto.PubkeyToAddress(key.PublicKey)
		log.Info().Str("address", addr.Hex()).Msg("Signing key loaded")
	}

	f.running = true
	return nil
}

// Stop shuts down the relay
func (f *Flashbots) Stop(ctx context.Context) {
	log.Info().Msg("Stopping Flashbots relay")
	f.running = false
}

// SendBundle submits a bundle to Flashbots
func (f *Flashbots) SendBundle(ctx context.Context, bundle *Bundle) (*BundleResponse, error) {
	if f.signingKey == nil {
		return nil, fmt.Errorf("signing key not configured")
	}

	// Create JSON-RPC request
	request := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "eth_sendBundle",
		"params":  []interface{}{bundle},
	}

	body, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}

	// Create HTTP request
	req, err := http.NewRequestWithContext(ctx, "POST", f.config.FlashbotsURL, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}

	req.Header.Set("Content-Type", "application/json")

	// Sign the request body
	signature, err := f.signPayload(body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("X-Flashbots-Signature", signature)

	// Send request with retries
	var resp *http.Response
	for i := 0; i <= f.config.MaxRetries; i++ {
		resp, err = f.httpClient.Do(req)
		if err == nil && resp.StatusCode == http.StatusOK {
			break
		}
		if i < f.config.MaxRetries {
			time.Sleep(100 * time.Millisecond)
		}
	}

	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("unexpected status code: %d", resp.StatusCode)
	}

	// Parse response
	var result struct {
		Result *BundleResponse `json:"result"`
		Error  *struct {
			Message string `json:"message"`
		} `json:"error"`
	}

	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, err
	}

	if result.Error != nil {
		return nil, fmt.Errorf("flashbots error: %s", result.Error.Message)
	}

	log.Info().
		Str("bundleHash", result.Result.BundleHash).
		Int("txCount", len(bundle.Txs)).
		Msg("Bundle submitted")

	return result.Result, nil
}

// SimulateBundle simulates a bundle
func (f *Flashbots) SimulateBundle(ctx context.Context, bundle *Bundle) (*SimulationResult, error) {
	if f.signingKey == nil {
		return nil, fmt.Errorf("signing key not configured")
	}

	request := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "eth_callBundle",
		"params":  []interface{}{bundle},
	}

	body, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}

	req, err := http.NewRequestWithContext(ctx, "POST", f.config.FlashbotsURL, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}

	req.Header.Set("Content-Type", "application/json")

	signature, err := f.signPayload(body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("X-Flashbots-Signature", signature)

	resp, err := f.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var result struct {
		Result *SimulationResult `json:"result"`
		Error  *struct {
			Message string `json:"message"`
		} `json:"error"`
	}

	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, err
	}

	if result.Error != nil {
		return nil, fmt.Errorf("simulation error: %s", result.Error.Message)
	}

	return result.Result, nil
}

// SimulationResult from eth_callBundle
type SimulationResult struct {
	BundleGasPrice    string               `json:"bundleGasPrice"`
	BundleHash        string               `json:"bundleHash"`
	CoinbaseDiff      string               `json:"coinbaseDiff"`
	EthSentToCoinbase string               `json:"ethSentToCoinbase"`
	GasFees           string               `json:"gasFees"`
	Results           []TxSimulationResult `json:"results"`
	StateBlockNumber  uint64               `json:"stateBlockNumber"`
	TotalGasUsed      uint64               `json:"totalGasUsed"`
}

// TxSimulationResult for individual tx
type TxSimulationResult struct {
	CoinbaseDiff      string         `json:"coinbaseDiff"`
	EthSentToCoinbase string         `json:"ethSentToCoinbase"`
	FromAddress       common.Address `json:"fromAddress"`
	GasFees           string         `json:"gasFees"`
	GasPrice          string         `json:"gasPrice"`
	GasUsed           uint64         `json:"gasUsed"`
	ToAddress         common.Address `json:"toAddress"`
	TxHash            common.Hash    `json:"txHash"`
	Value             string         `json:"value"`
	Error             string         `json:"error,omitempty"`
	Revert            string         `json:"revert,omitempty"`
}

func (f *Flashbots) signPayload(body []byte) (string, error) {
	// Hash the body
	hashedBody := crypto.Keccak256Hash(body).Hex()

	// Sign with EIP-191
	signature, err := crypto.Sign(
		crypto.Keccak256([]byte(fmt.Sprintf("\x19Ethereum Signed Message:\n%d%s", len(hashedBody), hashedBody))),
		f.signingKey,
	)
	if err != nil {
		return "", err
	}

	// Format: address:signature
	addr := crypto.PubkeyToAddress(f.signingKey.PublicKey)
	return fmt.Sprintf("%s:%s", addr.Hex(), hexutil.Encode(signature)), nil
}

// GetBundleStats retrieves bundle statistics
func (f *Flashbots) GetBundleStats(ctx context.Context, bundleHash string, blockNumber uint64) (map[string]interface{}, error) {
	request := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "flashbots_getBundleStats",
		"params": []interface{}{
			map[string]interface{}{
				"bundleHash":  bundleHash,
				"blockNumber": fmt.Sprintf("0x%x", blockNumber),
			},
		},
	}

	body, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}

	req, err := http.NewRequestWithContext(ctx, "POST", f.config.FlashbotsURL, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}

	req.Header.Set("Content-Type", "application/json")

	signature, err := f.signPayload(body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("X-Flashbots-Signature", signature)

	resp, err := f.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var result map[string]interface{}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, err
	}

	return result, nil
}
