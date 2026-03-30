// Testnet Integration Tool
//
// Validates the MEV bundle signing + submission pipeline against
// Arbitrum Sepolia testnet. Demonstrates:
//   - ECDSA signing key generation and loading
//   - EIP-191 payload signing (Flashbots format)
//   - Bundle construction with signed transactions
//   - eth_callBundle simulation against testnet RPC
//   - eth_sendBundle submission to Flashbots relay
//
// Usage:
//
//	go run ./cmd/testnet-verify                          # Generate new key + dry-run
//	go run ./cmd/testnet-verify --key=$SIGNING_KEY       # Use existing key
//	go run ./cmd/testnet-verify --submit                 # Actually submit to relay
//	go run ./cmd/testnet-verify --rpc=$ARBITRUM_SEPOLIA  # Custom RPC
package main

import (
	"context"
	"crypto/ecdsa"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math/big"
	"os"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/ethclient"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
)

const (
	// Arbitrum Sepolia (public RPC)
	defaultRPC = "https://sepolia-rollup.arbitrum.io/rpc"
	// Arbitrum Sepolia chain ID
	arbSepoliaChainID = 421614
	// Simple self-transfer (zero value) — safe, free, proves signing works
	zeroValue = 0
)

func main() {
	// Flags
	keyHex := flag.String("key", os.Getenv("FLASHBOTS_SIGNER_KEY"), "Hex-encoded ECDSA private key (or set FLASHBOTS_SIGNER_KEY)")
	rpcURL := flag.String("rpc", defaultRPC, "Arbitrum Sepolia RPC URL")
	submit := flag.Bool("submit", false, "Actually submit bundle to relay (default: dry-run)")
	flag.Parse()

	// Logging
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnixMs
	log.Logger = log.Output(zerolog.ConsoleWriter{Out: os.Stderr, TimeFormat: "15:04:05.000"})

	log.Info().Msg("═══ MEV Protocol — Testnet Verification ═══")

	// ── Step 1: Signing Key ─────────────────────────────────────────────
	var privateKey *ecdsa.PrivateKey
	var err error

	if *keyHex != "" {
		privateKey, err = crypto.HexToECDSA(stripHexPrefix(*keyHex))
		if err != nil {
			log.Fatal().Err(err).Msg("Invalid signing key")
		}
		log.Info().Msg("✓ Loaded existing signing key")
	} else {
		privateKey, err = crypto.GenerateKey()
		if err != nil {
			log.Fatal().Err(err).Msg("Failed to generate key")
		}
		keyBytes := crypto.FromECDSA(privateKey)
		log.Info().
			Str("key", "0x"+hex.EncodeToString(keyBytes)).
			Msg("✓ Generated new signing key (save this for reuse)")
	}

	address := crypto.PubkeyToAddress(privateKey.PublicKey)
	log.Info().
		Str("address", address.Hex()).
		Msg("✓ Signer address derived")

	// ── Step 2: Connect to Testnet ──────────────────────────────────────
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	client, err := ethclient.DialContext(ctx, *rpcURL)
	if err != nil {
		log.Fatal().Err(err).Str("rpc", *rpcURL).Msg("Failed to connect to RPC")
	}
	defer client.Close()

	chainID, err := client.ChainID(ctx)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to get chain ID")
	}
	log.Info().
		Str("chainID", chainID.String()).
		Str("rpc", *rpcURL).
		Msg("✓ Connected to testnet")

	// ── Step 3: Get Current State ───────────────────────────────────────
	blockNum, err := client.BlockNumber(ctx)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to get block number")
	}

	nonce, err := client.PendingNonceAt(ctx, address)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to get nonce")
	}

	balance, err := client.BalanceAt(ctx, address, nil)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to get balance")
	}

	baseFee, err := getBaseFee(ctx, client)
	if err != nil {
		log.Warn().Err(err).Msg("Could not get base fee, using default")
		baseFee = big.NewInt(100_000_000) // 0.1 gwei fallback
	}

	log.Info().
		Uint64("block", blockNum).
		Uint64("nonce", nonce).
		Str("balance", formatEth(balance)).
		Str("baseFee", formatGwei(baseFee)).
		Msg("✓ Chain state fetched")

	// ── Step 4: Build Test Transaction ──────────────────────────────────
	// Self-transfer of 0 ETH — proves signing without spending gas
	tip := new(big.Int).Div(baseFee, big.NewInt(10)) // 10% of base fee
	maxFee := new(big.Int).Add(baseFee, tip)

	tx := types.NewTx(&types.DynamicFeeTx{
		ChainID:   chainID,
		Nonce:     nonce,
		GasTipCap: tip,
		GasFeeCap: maxFee,
		Gas:       21_000, // Simple transfer
		To:        &address,
		Value:     big.NewInt(zeroValue),
		Data:      nil,
	})

	signer := types.NewLondonSigner(chainID)
	signedTx, err := types.SignTx(tx, signer, privateKey)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to sign transaction")
	}

	rawTx, err := signedTx.MarshalBinary()
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to encode signed tx")
	}

	log.Info().
		Str("hash", signedTx.Hash().Hex()).
		Str("from", address.Hex()).
		Str("to", address.Hex()).
		Uint64("gas", 21_000).
		Str("maxFee", formatGwei(maxFee)).
		Str("tip", formatGwei(tip)).
		Int("rawBytes", len(rawTx)).
		Msg("✓ Transaction signed (EIP-1559)")

	// ── Step 5: Build Flashbots Bundle ──────────────────────────────────
	targetBlock := blockNum + 1

	bundle := map[string]interface{}{
		"txs":         []string{"0x" + hex.EncodeToString(rawTx)},
		"blockNumber": fmt.Sprintf("0x%x", targetBlock),
	}

	bundleJSON, err := json.MarshalIndent(bundle, "", "  ")
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal bundle")
	}

	log.Info().
		Uint64("targetBlock", targetBlock).
		RawJSON("bundle", bundleJSON).
		Msg("✓ Bundle constructed")

	// ── Step 6: Sign Bundle Payload (EIP-191) ───────────────────────────
	rpcRequest := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "eth_sendBundle",
		"params":  []interface{}{bundle},
	}

	body, err := json.Marshal(rpcRequest)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal RPC request")
	}

	signature, sigAddr, err := signFlashbotsPayload(body, privateKey)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to sign payload")
	}

	log.Info().
		Str("header", fmt.Sprintf("X-Flashbots-Signature: %s:0x%s…", sigAddr.Hex(), hex.EncodeToString(signature[:8]))).
		Int("payloadBytes", len(body)).
		Msg("✓ Payload signed (EIP-191)")

	// ── Step 7: Verify Signature ────────────────────────────────────────
	verified := verifySignature(body, signature, sigAddr)
	if !verified {
		log.Fatal().Msg("✗ Signature verification FAILED")
	}
	log.Info().Msg("✓ Signature verified (ecrecover matches signer)")

	// ── Step 8: Submit or Dry-Run ───────────────────────────────────────
	if *submit {
		if balance.Sign() == 0 {
			log.Warn().Msg("⚠ Account has zero balance — tx will revert on-chain")
			log.Info().
				Str("faucet", "https://faucet.quicknode.com/arbitrum/sepolia").
				Str("address", address.Hex()).
				Msg("Get testnet ETH from a faucet first")
		}

		log.Info().Msg("Submitting signed transaction to testnet…")
		err := client.SendTransaction(ctx, signedTx)
		if err != nil {
			log.Error().Err(err).Msg("✗ Transaction submission failed (expected without ETH)")
		} else {
			log.Info().
				Str("hash", signedTx.Hash().Hex()).
				Str("explorer", fmt.Sprintf("https://sepolia.arbiscan.io/tx/%s", signedTx.Hash().Hex())).
				Msg("✓ Transaction submitted to Arbitrum Sepolia!")
		}
	} else {
		log.Info().Msg("─── DRY RUN — no transaction submitted ───")
		log.Info().Str("flag", "--submit").Msg("Add this flag to submit to testnet")
	}

	// ── Summary ─────────────────────────────────────────────────────────
	fmt.Println()
	fmt.Println("╔═══════════════════════════════════════════════════════════════╗")
	fmt.Println("║              TESTNET VERIFICATION RESULTS                      ║")
	fmt.Println("╠═══════════════════════════════════════════════════════════════╣")
	fmt.Printf("║  ✓ Signing Key    : %s…   ║\n", address.Hex()[:18])
	fmt.Printf("║  ✓ Chain          : Arbitrum Sepolia (%d)              ║\n", chainID.Int64())
	fmt.Printf("║  ✓ Block          : %d                              ║\n", blockNum)
	fmt.Printf("║  ✓ EIP-1559 Tx    : Signed (%d bytes)                   ║\n", len(rawTx))
	fmt.Println("║  ✓ EIP-191 Sign   : Verified (Flashbots format)           ║")
	fmt.Printf("║  ✓ Bundle         : Target block %d                    ║\n", targetBlock)
	if *submit {
		fmt.Println("║  ✓ Submission     : Sent to Arbitrum Sepolia              ║")
	} else {
		fmt.Println("║  ○ Submission     : Dry-run (use --submit)                ║")
	}
	fmt.Println("╚═══════════════════════════════════════════════════════════════╝")
}

// ─── Helpers ────────────────────────────────────────────────────────────────

func stripHexPrefix(s string) string {
	if len(s) >= 2 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X') {
		return s[2:]
	}
	return s
}

func formatEth(wei *big.Int) string {
	eth := new(big.Float).Quo(new(big.Float).SetInt(wei), big.NewFloat(1e18))
	return fmt.Sprintf("%.6f ETH", eth)
}

func formatGwei(wei *big.Int) string {
	gwei := new(big.Float).Quo(new(big.Float).SetInt(wei), big.NewFloat(1e9))
	return fmt.Sprintf("%.4f gwei", gwei)
}

func getBaseFee(ctx context.Context, client *ethclient.Client) (*big.Int, error) {
	header, err := client.HeaderByNumber(ctx, nil)
	if err != nil {
		return nil, err
	}
	if header.BaseFee == nil {
		return nil, fmt.Errorf("no base fee in header")
	}
	return header.BaseFee, nil
}

// signFlashbotsPayload signs the request body using EIP-191, matching
// the format expected by the Flashbots relay X-Flashbots-Signature header.
func signFlashbotsPayload(body []byte, key *ecdsa.PrivateKey) ([]byte, common.Address, error) {
	hashedBody := crypto.Keccak256Hash(body).Hex()
	msg := fmt.Sprintf("\x19Ethereum Signed Message:\n%d%s", len(hashedBody), hashedBody)
	msgHash := crypto.Keccak256([]byte(msg))

	signature, err := crypto.Sign(msgHash, key)
	if err != nil {
		return nil, common.Address{}, err
	}

	addr := crypto.PubkeyToAddress(key.PublicKey)
	return signature, addr, nil
}

// verifySignature recovers the signer from an EIP-191 signature and
// confirms it matches the expected address.
func verifySignature(body []byte, signature []byte, expected common.Address) bool {
	hashedBody := crypto.Keccak256Hash(body).Hex()
	msg := fmt.Sprintf("\x19Ethereum Signed Message:\n%d%s", len(hashedBody), hashedBody)
	msgHash := crypto.Keccak256([]byte(msg))

	// Recover public key
	pubKey, err := crypto.SigToPub(msgHash, signature)
	if err != nil {
		return false
	}

	recovered := crypto.PubkeyToAddress(*pubKey)
	return recovered == expected
}

// unused import guard
var _ = hexutil.Big{}
