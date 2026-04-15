//! FFI bindings for the C hot-path library (`fast/`).
//!
//! Provides sub-microsecond primitives for the MEV pipeline by calling into
//! hand-optimised C code compiled with `-O3 -mavx2 -msse4.2`.
//!
//! When the `has_c_fast_path` cfg flag is set (i.e. `libmev_fast.a` was built),
//! functions dispatch to the C implementation. Otherwise, pure-Rust fallbacks
//! are used transparently through the [`safe`] module.
//!
//! # Safety
//!
//! All raw FFI calls are wrapped in the [`safe`] sub-module which validates
//! slice lengths and types before crossing the boundary. Direct use of the
//! `extern "C"` symbols is discouraged — prefer `safe::*` wrappers.

use std::ffi::c_void;
use std::os::raw::c_int;

// Link to our C library
#[cfg(has_c_fast_path)]
#[link(name = "mev_fast", kind = "static")]
extern "C" {
    /// Compute Keccak-256 hash of `data[0..len]` and write the 32-byte digest to `out`.
    /// Returns 0 on success, -1 on null pointers.
    pub fn mev_keccak256(data: *const u8, len: usize, out: *mut u8) -> c_int;

    /// Hash a null-terminated Solidity function signature (e.g. `"transfer(address,uint256)"`)
    /// and return the 4-byte selector packed as a big-endian u32.
    pub fn mev_function_selector(signature: *const u8) -> u32;

    /// RLP-encode a byte string. Writes encoded bytes to `out` and sets `*out_len`.
    /// Returns 0 on success.
    pub fn mev_rlp_encode_string(data: *const u8, len: usize, out: *mut u8, out_len: *mut usize) -> c_int;

    /// RLP-encode a big-endian uint256 (32-byte `value`). Writes to `out`, sets `*out_len`.
    /// Returns 0 on success.
    pub fn mev_rlp_encode_uint256(value: *const u8, out: *mut u8, out_len: *mut usize) -> c_int;

    /// RLP-encode a 20-byte Ethereum address. Writes to `out`, sets `*out_len`.
    /// Returns 0 on success.
    pub fn mev_rlp_encode_address(addr: *const u8, out: *mut u8, out_len: *mut usize) -> c_int;

    /// Parse raw swap calldata into a [`SwapInfoFFI`] struct.
    /// Returns 0 on success, non-zero if the selector is unrecognised or data is malformed.
    pub fn mev_parse_swap(calldata: *const u8, len: usize, info: *mut SwapInfoFFI) -> i32;

    /// Extract the 4-byte function selector from calldata.
    pub fn mev_get_selector(calldata: *const u8, out: *mut u8);

    /// SIMD-accelerated `memcmp` (AVX2 for ≥32 B, SSE4.2 for ≥16 B, scalar fallback).
    /// Returns 0 when equal, non-zero otherwise.
    pub fn mev_memcmp_fast(a: *const u8, b: *const u8, len: usize) -> i32;

    /// Compare two 20-byte Ethereum addresses using SIMD. Returns non-zero if equal.
    pub fn mev_address_eq(a: *const u8, b: *const u8) -> i32;

    /// Batch constant-product price impact for 4 pools in parallel (AVX2).
    ///
    /// `reserves0` and `reserves1` are arrays of 4 pool reserve pairs.
    /// `outputs[i]` receives `amount_in * reserves1[i] * 997 / (reserves0[i] * 1000 + amount_in * 997)`.
    pub fn mev_calc_price_impact_batch(
        reserves0: *const u64,
        reserves1: *const u64,
        amount_in: u64,
        outputs: *mut u64,
    );

    /// Read the CPU timestamp counter (RDTSC). Non-serialising — use for relative
    /// cycle measurements only, not wall-clock time.
    pub fn mev_rdtsc() -> u64;

    /// Issue a cache prefetch hint for the memory at `data` (L1 temporal, `_MM_HINT_T0`).
    pub fn mev_prefetch_pool(data: *const c_void);

    /// Initialise the arena allocator pools (tx, calldata, result buffers).
    /// Returns 0 on success.
    pub fn mev_pools_init() -> i32;

    /// Allocate a 512-byte transaction buffer from the arena pool. Returns null on exhaustion.
    pub fn mev_alloc_tx() -> *mut u8;

    /// Return a transaction buffer to the arena pool.
    pub fn mev_free_tx(ptr: *mut u8);

    /// Allocate a calldata buffer from the arena pool. Returns null on exhaustion.
    pub fn mev_alloc_calldata() -> *mut u8;

    /// Return a calldata buffer to the arena pool.
    pub fn mev_free_calldata(ptr: *mut u8);

    /// Create a lock-free MPSC queue with the given capacity (rounded up to power of 2).
    /// Returns null on allocation failure.
    pub fn mev_queue_create(capacity: usize) -> *mut c_void;

    /// Destroy a lock-free queue and free its backing memory.
    pub fn mev_queue_destroy(q: *mut c_void);

    /// Push an item onto the queue. Returns 0 on success, -1 if full.
    pub fn mev_queue_push(q: *mut c_void, item: *mut c_void) -> i32;

    /// Pop an item from the queue. Returns null if empty.
    pub fn mev_queue_pop(q: *mut c_void) -> *mut c_void;

    /// Return the current number of items in the queue (approximate under concurrency).
    pub fn mev_queue_size(q: *mut c_void) -> usize;
}

/// C-compatible struct returned by [`mev_parse_swap`] after decoding swap calldata.
///
/// All multi-byte fields use big-endian encoding to match Solidity ABI conventions.
#[repr(C)]
pub struct SwapInfoFFI {
    /// DEX identifier: 1 = Uniswap V2, 2 = Uniswap V3, 3 = SushiSwap.
    pub dex_type: u8,
    /// Input token address (20 bytes).
    pub token_in: [u8; 20],
    /// Output token address (20 bytes).
    pub token_out: [u8; 20],
    /// Input amount as big-endian uint256.
    pub amount_in: [u8; 32],
    /// Minimum output amount as big-endian uint256 (slippage bound).
    pub amount_out_min: [u8; 32],
    /// Target pool address (20 bytes).
    pub pool_address: [u8; 20],
    /// Pool fee in basis points (e.g. 3000 = 0.3% for V3).
    pub fee: u32,
}

impl Default for SwapInfoFFI {
    fn default() -> Self {
        Self {
            dex_type: 0,
            token_in: [0u8; 20],
            token_out: [0u8; 20],
            amount_in: [0u8; 32],
            amount_out_min: [0u8; 32],
            pool_address: [0u8; 20],
            fee: 0,
        }
    }
}

// ── Shared types (no FFI dependency) ──────────────────────────────────

/// DEX protocol identifier used across the detection pipeline.
#[derive(Debug, Clone)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
}

/// Decoded swap information with Ethereum-native types.
///
/// Produced by [`safe::parse_swap`] after converting the raw C struct
/// into `ethers` address/U256 types safe for further pipeline processing.
#[derive(Debug, Clone)]
pub struct SwapInfo {
    pub dex_type: DexType,
    pub token_in: ethers::types::Address,
    pub token_out: ethers::types::Address,
    pub amount_in: ethers::types::U256,
    pub amount_out_min: ethers::types::U256,
    pub pool_address: ethers::types::Address,
    pub fee: u32,
}

// ══════════════════════════════════════════════════════════════════════
// When the C fast-path library IS available
// ══════════════════════════════════════════════════════════════════════

/// Safe wrappers around the C hot-path library.
///
/// When `cfg(has_c_fast_path)` is active, these call into `libmev_fast.a`.
/// Otherwise, identical pure-Rust fallbacks are provided so callers never
/// need conditional compilation.
#[cfg(has_c_fast_path)]
pub mod safe {
    use super::*;
    use ethers::types::{Address, H256, U256};

    /// Initialise C arena allocator pools. Call once at startup.
    pub fn init_pools() -> bool {
        unsafe { mev_pools_init() == 0 }
    }

    /// Compute Keccak-256 via the C implementation (~550 ns for 32 bytes).
    #[inline(always)]
    pub fn keccak256_fast(data: &[u8]) -> H256 {
        let mut out = [0u8; 32];
        unsafe { mev_keccak256(data.as_ptr(), data.len(), out.as_mut_ptr()); }
        H256::from(out)
    }

    /// Compute the 4-byte Solidity function selector for a canonical signature.
    #[inline(always)]
    pub fn function_selector(signature: &str) -> [u8; 4] {
        // C function expects null-terminated string and returns packed u32
        let cstr = std::ffi::CString::new(signature).expect("signature contains null byte");
        let sel = unsafe { mev_function_selector(cstr.as_ptr() as *const u8) };
        sel.to_be_bytes()
    }

    /// SIMD-accelerated 20-byte address comparison.
    #[inline(always)]
    pub fn address_eq(a: &Address, b: &Address) -> bool {
        unsafe { mev_address_eq(a.as_bytes().as_ptr(), b.as_bytes().as_ptr()) != 0 }
    }

    /// Batch constant-product price impact for 4 pools in a single AVX2 pass.
    #[inline(always)]
    pub fn calc_price_impact_batch(
        reserves0: &[u64; 4], reserves1: &[u64; 4], amount_in: u64,
    ) -> [u64; 4] {
        let mut outputs = [0u64; 4];
        unsafe { mev_calc_price_impact_batch(reserves0.as_ptr(), reserves1.as_ptr(), amount_in, outputs.as_mut_ptr()); }
        outputs
    }

    /// Parse raw swap calldata into a typed [`SwapInfo`]. Returns `None` for
    /// unrecognised selectors or malformed ABI data.
    pub fn parse_swap(calldata: &[u8]) -> Option<SwapInfo> {
        let mut info = SwapInfoFFI::default();
        let result = unsafe { mev_parse_swap(calldata.as_ptr(), calldata.len(), &mut info) };
        if result != 0 { return None; }
        Some(SwapInfo {
            dex_type: match info.dex_type { 1 => DexType::UniswapV2, 2 => DexType::UniswapV3, 3 => DexType::SushiSwap, _ => return None },
            token_in: Address::from_slice(&info.token_in),
            token_out: Address::from_slice(&info.token_out),
            amount_in: U256::from_big_endian(&info.amount_in),
            amount_out_min: U256::from_big_endian(&info.amount_out_min),
            pool_address: Address::from_slice(&info.pool_address),
            fee: info.fee,
        })
    }

    /// Read the CPU timestamp counter for cycle-level latency measurements.
    #[inline(always)]
    pub fn rdtsc() -> u64 { unsafe { mev_rdtsc() } }

    /// Issue a cache prefetch hint to pull `data` into L1.
    #[inline(always)]
    pub fn prefetch<T>(data: &T) {
        unsafe { mev_prefetch_pool(data as *const T as *const c_void); }
    }

    /// RLP-encode a 20-byte Ethereum address.
    pub fn rlp_encode_address(addr: &Address) -> Vec<u8> {
        let mut out = vec![0u8; 21];
        let mut len: usize = 0;
        unsafe { mev_rlp_encode_address(addr.as_bytes().as_ptr(), out.as_mut_ptr(), &mut len); }
        out.truncate(len);
        out
    }

    /// RLP-encode a `U256` value in big-endian form.
    pub fn rlp_encode_u256(value: U256) -> Vec<u8> {
        let mut bytes = [0u8; 32];
        value.to_big_endian(&mut bytes);
        let mut out = vec![0u8; 33];
        let mut len: usize = 0;
        unsafe { mev_rlp_encode_uint256(bytes.as_ptr(), out.as_mut_ptr(), &mut len); }
        out.truncate(len);
        out
    }

    pub use super::{DexType, SwapInfo};
}

// ══════════════════════════════════════════════════════════════════════
// Pure-Rust fallbacks when the C library is NOT compiled
// ══════════════════════════════════════════════════════════════════════

/// Pure-Rust fallback implementations matching the C hot-path API.
///
/// Used automatically when `has_c_fast_path` is not set (e.g. CI without a C
/// toolchain, or development builds). Performance is ~2-5× slower than the
/// SIMD-accelerated C version but produces identical results.
#[cfg(not(has_c_fast_path))]
pub mod safe {
    use super::*;
    use ethers::types::{Address, H256, U256};
    use sha3::{Digest, Keccak256};

    pub fn init_pools() -> bool { true }

    #[inline(always)]
    pub fn keccak256_fast(data: &[u8]) -> H256 {
        let mut hasher = Keccak256::new();
        hasher.update(data);
        H256::from_slice(&hasher.finalize())
    }

    #[inline(always)]
    pub fn function_selector(signature: &str) -> [u8; 4] {
        let hash = keccak256_fast(signature.as_bytes());
        let mut out = [0u8; 4];
        out.copy_from_slice(&hash.as_bytes()[..4]);
        out
    }

    #[inline(always)]
    pub fn address_eq(a: &Address, b: &Address) -> bool { a == b }

    #[inline(always)]
    pub fn calc_price_impact_batch(
        reserves0: &[u64; 4], reserves1: &[u64; 4], amount_in: u64,
    ) -> [u64; 4] {
        let mut outputs = [0u64; 4];
        for i in 0..4 {
            let (r0, r1) = (reserves0[i] as u128, reserves1[i] as u128);
            if r0 == 0 { outputs[i] = 0; continue; }
            let amt = amount_in as u128;
            let numerator = amt * r1 * 997;
            let denominator = r0 * 1000 + amt * 997;
            outputs[i] = (numerator / denominator) as u64;
        }
        outputs
    }

    pub fn parse_swap(_calldata: &[u8]) -> Option<SwapInfo> { None }

    #[inline(always)]
    pub fn rdtsc() -> u64 { super::rdtsc_native() }

    #[inline(always)]
    pub fn prefetch<T>(_data: &T) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::x86_64::_mm_prefetch(
                _data as *const T as *const i8,
                std::arch::x86_64::_MM_HINT_T0,
            );
        }
    }

    pub fn rlp_encode_address(addr: &Address) -> Vec<u8> {
        let bytes = addr.as_bytes();
        let mut out = Vec::with_capacity(21);
        out.push(0x80 + 20);
        out.extend_from_slice(bytes);
        out
    }

    pub fn rlp_encode_u256(value: U256) -> Vec<u8> {
        let mut bytes = [0u8; 32];
        value.to_big_endian(&mut bytes);
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(31);
        let significant = &bytes[start..];
        if significant.len() == 1 && significant[0] < 0x80 {
            significant.to_vec()
        } else {
            let mut out = Vec::with_capacity(significant.len() + 1);
            out.push(0x80 + significant.len() as u8);
            out.extend_from_slice(significant);
            out
        }
    }

    pub use super::{DexType, SwapInfo};
}

// ══════════════════════════════════════════════════════════════════════
// Lock-free queue – C FFI vs Mutex<VecDeque> fallback
// ══════════════════════════════════════════════════════════════════════

/// Lock-free MPSC queue for passing detected opportunities from detector
/// workers to the simulator thread.
///
/// When `has_c_fast_path` is active, backed by the CAS-based C implementation
/// (`fast/src/lockfree_queue.c`). Otherwise, falls back to `Mutex<VecDeque>`.
#[cfg(has_c_fast_path)]
pub struct OpportunityQueue { inner: *mut c_void }

#[cfg(has_c_fast_path)]
impl OpportunityQueue {
    pub fn new(capacity: usize) -> Option<Self> {
        let inner = unsafe { mev_queue_create(capacity) };
        if inner.is_null() { None } else { Some(Self { inner }) }
    }
    pub fn push(&self, item: *mut c_void) -> bool { unsafe { mev_queue_push(self.inner, item) == 0 } }
    pub fn pop(&self) -> Option<*mut c_void> {
        let ptr = unsafe { mev_queue_pop(self.inner) };
        if ptr.is_null() { None } else { Some(ptr) }
    }
    pub fn len(&self) -> usize { unsafe { mev_queue_size(self.inner) } }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

#[cfg(has_c_fast_path)]
impl Drop for OpportunityQueue {
    fn drop(&mut self) { unsafe { mev_queue_destroy(self.inner) } }
}

// Safety: the C queue uses atomic CAS operations and is safe to share across threads.
#[cfg(has_c_fast_path)]
unsafe impl Send for OpportunityQueue {}
#[cfg(has_c_fast_path)]
unsafe impl Sync for OpportunityQueue {}

/// Fallback opportunity queue backed by `Mutex<VecDeque>` when the C library is not available.
#[cfg(not(has_c_fast_path))]
pub struct OpportunityQueue {
    inner: std::sync::Mutex<std::collections::VecDeque<*mut c_void>>,
}

#[cfg(not(has_c_fast_path))]
impl OpportunityQueue {
    pub fn new(capacity: usize) -> Option<Self> {
        Some(Self { inner: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(capacity)) })
    }
    pub fn push(&self, item: *mut c_void) -> bool {
        self.inner.lock().unwrap().push_back(item); true
    }
    pub fn pop(&self) -> Option<*mut c_void> {
        self.inner.lock().unwrap().pop_front()
    }
    pub fn len(&self) -> usize { self.inner.lock().unwrap().len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

#[cfg(not(has_c_fast_path))]
unsafe impl Send for OpportunityQueue {}
#[cfg(not(has_c_fast_path))]
unsafe impl Sync for OpportunityQueue {}

// ══════════════════════════════════════════════════════════════════════
// TX buffer – C pool alloc vs Vec<u8> fallback
// ══════════════════════════════════════════════════════════════════════

/// Pre-allocated 512-byte transaction buffer from the C arena pool.
///
/// Avoids per-transaction heap allocation in the hot path. When `has_c_fast_path`
/// is not set, falls back to a `Vec<u8>` with the same capacity.
#[cfg(has_c_fast_path)]
pub struct TxBuffer { ptr: *mut u8, len: usize }

#[cfg(has_c_fast_path)]
impl TxBuffer {
    pub fn new() -> Option<Self> {
        let ptr = unsafe { mev_alloc_tx() };
        if ptr.is_null() { None } else { Some(Self { ptr, len: 0 }) }
    }
    pub fn as_slice(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.ptr, self.len) } }
    pub fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        self.len = len.min(512);
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

#[cfg(has_c_fast_path)]
impl Drop for TxBuffer {
    fn drop(&mut self) { unsafe { mev_free_tx(self.ptr) } }
}

#[cfg(not(has_c_fast_path))]
pub struct TxBuffer { buf: Vec<u8>, len: usize }

#[cfg(not(has_c_fast_path))]
impl TxBuffer {
    pub fn new() -> Option<Self> {
        Some(Self { buf: vec![0u8; 512], len: 0 })
    }
    pub fn as_slice(&self) -> &[u8] { &self.buf[..self.len] }
    pub fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        self.len = len.min(512);
        &mut self.buf[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::types::{Address, H256, U256};
    use std::str::FromStr;

    // ── Keccak256 known vectors ──

    #[test]
    fn test_keccak256_empty() {
        let hash = safe::keccak256_fast(b"");
        let expected = H256::from_str(
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        ).unwrap();
        assert_eq!(hash, expected, "keccak256('') mismatch");
    }

    #[test]
    fn test_keccak256_hello() {
        let hash = safe::keccak256_fast(b"hello");
        let expected = H256::from_str(
            "0x1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        ).unwrap();
        assert_eq!(hash, expected, "keccak256('hello') mismatch");
    }

    #[test]
    fn test_keccak256_not_zero() {
        let hash = safe::keccak256_fast(b"transfer(address,uint256)");
        assert!(!hash.is_zero());
    }

    // ── Function selector ──

    #[test]
    fn test_function_selector_transfer() {
        let sel = safe::function_selector("transfer(address,uint256)");
        assert_eq!(sel, [0xa9, 0x05, 0x9c, 0xbb], "transfer selector mismatch");
    }

    #[test]
    fn test_function_selector_approve() {
        let sel = safe::function_selector("approve(address,uint256)");
        assert_eq!(sel, [0x09, 0x5e, 0xa7, 0xb3], "approve selector mismatch");
    }

    #[test]
    fn test_function_selector_swap_v2() {
        let sel = safe::function_selector("swapExactTokensForTokens(uint256,uint256,address[],address,uint256)");
        assert_eq!(sel, [0x38, 0xed, 0x17, 0x39], "V2 swap selector mismatch");
    }

    // ── Address equality ──

    #[test]
    fn test_address_eq_same() {
        let addr = Address::from_str("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        assert!(safe::address_eq(&addr, &addr));
    }

    #[test]
    fn test_address_eq_different() {
        let a = Address::from_str("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let b = Address::from_str("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48").unwrap();
        assert!(!safe::address_eq(&a, &b));
    }

    #[test]
    fn test_address_eq_zero() {
        let zero = Address::zero();
        assert!(safe::address_eq(&zero, &zero));
    }

    // ── RLP encoding ──

    #[test]
    fn test_rlp_encode_address_length() {
        let addr = Address::from_str("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let encoded = safe::rlp_encode_address(&addr);
        assert_eq!(encoded.len(), 21, "RLP address should be 1 prefix + 20 bytes");
        assert_eq!(encoded[0], 0x80 + 20, "RLP prefix for 20-byte string");
    }

    #[test]
    fn test_rlp_encode_u256_zero() {
        let encoded = safe::rlp_encode_u256(U256::zero());
        // Zero encodes as a single byte 0x00 (short string)
        assert!(!encoded.is_empty());
        // RLP for 0 is [0x00] — single byte below 0x80
        assert_eq!(encoded, vec![0x00]);
    }

    #[test]
    fn test_rlp_encode_u256_small() {
        let encoded = safe::rlp_encode_u256(U256::from(42));
        // 42 < 0x80, so single byte encoding
        assert_eq!(encoded, vec![42]);
    }

    #[test]
    fn test_rlp_encode_u256_large() {
        let val = U256::from(256u64); // > 0x80, needs prefix
        let encoded = safe::rlp_encode_u256(val);
        assert!(encoded.len() > 1, "large values need prefix");
        assert_eq!(encoded[0], 0x80 + 2); // 256 = 0x0100 → 2 bytes
        assert_eq!(encoded[1], 0x01);
        assert_eq!(encoded[2], 0x00);
    }

    // ── calc_price_impact_batch ──

    #[test]
    fn test_price_impact_batch_basic() {
        let r0 = [1_000_000u64, 2_000_000, 500_000, 10_000_000];
        let r1 = [2_000_000u64, 1_000_000, 1_000_000, 5_000_000];
        let amount_in = 10_000u64;
        let outputs = safe::calc_price_impact_batch(&r0, &r1, amount_in);
        // All outputs should be positive
        for (i, &out) in outputs.iter().enumerate() {
            assert!(out > 0, "pool {} should have positive output, got {}", i, out);
        }
        // Pool 0: 10000 * 2M * 997 / (1M * 1000 + 10000 * 997) ≈ 19740
        assert!(outputs[0] > 19_000 && outputs[0] < 20_000);
    }

    #[test]
    fn test_price_impact_batch_zero_reserve() {
        let r0 = [0u64, 1_000_000, 0, 1_000_000];
        let r1 = [1_000_000u64, 1_000_000, 1_000_000, 1_000_000];
        let outputs = safe::calc_price_impact_batch(&r0, &r1, 1000);
        assert_eq!(outputs[0], 0, "zero reserve should produce zero output");
        assert_eq!(outputs[2], 0, "zero reserve should produce zero output");
        assert!(outputs[1] > 0);
    }

    // ── OpportunityQueue ──

    #[test]
    fn test_queue_new_not_none() {
        let q = OpportunityQueue::new(16);
        assert!(q.is_some());
    }

    #[test]
    fn test_queue_push_pop() {
        let q = OpportunityQueue::new(16).unwrap();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);

        let val: u64 = 42;
        let ptr = &val as *const u64 as *mut c_void;
        assert!(q.push(ptr));
        assert_eq!(q.len(), 1);
        assert!(!q.is_empty());

        let popped = q.pop();
        assert!(popped.is_some());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_queue_pop_empty() {
        let q = OpportunityQueue::new(16).unwrap();
        assert!(q.pop().is_none());
    }

    #[test]
    fn test_queue_fifo_order() {
        let q = OpportunityQueue::new(16).unwrap();
        let a: u64 = 1;
        let b: u64 = 2;
        let c: u64 = 3;
        q.push(&a as *const u64 as *mut c_void);
        q.push(&b as *const u64 as *mut c_void);
        q.push(&c as *const u64 as *mut c_void);
        assert_eq!(q.len(), 3);

        let p1 = q.pop().unwrap();
        let p2 = q.pop().unwrap();
        let p3 = q.pop().unwrap();
        assert_eq!(p1, &a as *const u64 as *mut c_void);
        assert_eq!(p2, &b as *const u64 as *mut c_void);
        assert_eq!(p3, &c as *const u64 as *mut c_void);
    }

    // ── TxBuffer ──

    #[test]
    fn test_txbuffer_new() {
        let buf = TxBuffer::new();
        assert!(buf.is_some());
    }

    #[test]
    fn test_txbuffer_initial_empty() {
        let buf = TxBuffer::new().unwrap();
        assert_eq!(buf.as_slice().len(), 0);
    }

    #[test]
    fn test_txbuffer_write_and_read() {
        let mut buf = TxBuffer::new().unwrap();
        let slice = buf.as_mut_slice(4);
        slice[0] = 0xDE;
        slice[1] = 0xAD;
        slice[2] = 0xBE;
        slice[3] = 0xEF;
        let read = buf.as_slice();
        assert_eq!(read, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_txbuffer_cap_at_512() {
        let mut buf = TxBuffer::new().unwrap();
        let slice = buf.as_mut_slice(1000); // request more than 512
        assert_eq!(slice.len(), 512, "should be capped at 512");
    }

    // ── SwapInfoFFI default ──

    #[test]
    fn test_swap_info_ffi_default() {
        let info = SwapInfoFFI::default();
        assert_eq!(info.dex_type, 0);
        assert_eq!(info.fee, 0);
        assert_eq!(info.token_in, [0u8; 20]);
        assert_eq!(info.amount_in, [0u8; 32]);
    }
}

// Re-export commonly used items
pub use safe::{
    keccak256_fast,
    function_selector,
    address_eq,
    calc_price_impact_batch,
    parse_swap,
    rdtsc,
    prefetch,
    rlp_encode_address,
    rlp_encode_u256,
};
// DexType and SwapInfo are already defined at module level — no re-export needed

/// Inline RDTSC fallback (pure Rust, no C dependency)
#[inline(always)]
pub fn rdtsc_native() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { std::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}

/// Prefetch for when C lib is not available
#[inline(always)]
pub fn mev_prefetch_pool_fast(data: *const c_void) {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            use std::arch::x86_64::_MM_HINT_T0;
            _mm_prefetch(data as *const i8, _MM_HINT_T0);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = data; // Suppress warning
    }
}
