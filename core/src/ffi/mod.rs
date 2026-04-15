//! FFI bindings for C hot path - PRODUCTION OPTIMIZED

pub mod hot_path;
pub mod simulator;

use std::ffi::c_void;
use std::os::raw::c_int;

// Re-export hot_path items
pub use hot_path::{rdtsc_native, TxBuffer, OpportunityQueue, SwapInfo};

#[cfg(has_c_fast_path)]
#[link(name = "mev_fast", kind = "static")]
extern "C" {
    // Keccak256
    pub fn mev_keccak256(data: *const u8, len: usize, out: *mut u8) -> c_int;
    pub fn mev_function_selector(signature: *const u8) -> u32;
    
    // RLP encoding
    pub fn mev_rlp_encode_string(data: *const u8, len: usize, out: *mut u8, out_len: *mut usize) -> c_int;
    pub fn mev_rlp_encode_uint256(value: *const u8, out: *mut u8, out_len: *mut usize) -> c_int;
    pub fn mev_rlp_encode_address(addr: *const u8, out: *mut u8, out_len: *mut usize) -> c_int;
    
    // SIMD utils
    pub fn mev_memcmp_fast(a: *const u8, b: *const u8, len: usize) -> i32;
    pub fn mev_address_eq(a: *const u8, b: *const u8) -> i32;
    pub fn mev_calc_price_impact_batch(
        reserves0: *const u64,
        reserves1: *const u64,
        amount_in: u64,
        outputs: *mut u64,
    );
    
    // Memory pool
    pub fn mev_pools_init() -> i32;
    pub fn mev_alloc_tx() -> *mut u8;
    pub fn mev_free_tx(ptr: *mut u8);
    pub fn mev_alloc_calldata() -> *mut u8;
    pub fn mev_free_calldata(ptr: *mut u8);
    
    // Lock-free queue
    pub fn mev_queue_create(capacity: usize) -> *mut c_void;
    pub fn mev_queue_destroy(q: *mut c_void);
    pub fn mev_queue_push(q: *mut c_void, item: *mut c_void) -> i32;
    pub fn mev_queue_pop(q: *mut c_void) -> *mut c_void;
    pub fn mev_queue_size(q: *mut c_void) -> usize;
}

/// Safe wrapper for Keccak256.
///
/// When the C hot-path library is compiled in (`has_c_fast_path`), the SIMD-
/// accelerated C implementation is used by default. Set `MEV_DISABLE_FFI=1`
/// to force the pure-Rust fallback (useful for debugging or portability).
///
/// Without the C library, always uses the Rust `sha3` crate.
pub fn keccak256(input: &[u8]) -> [u8; 32] {
    let mut output = [0u8; 32];

    #[cfg(has_c_fast_path)]
    if !disable_c_fast_path() {
        unsafe {
            mev_keccak256(input.as_ptr(), input.len(), output.as_mut_ptr());
        }
        return output;
    }
    
    use sha3::{Digest, Keccak256 as K256};
    let mut hasher = K256::new();
    hasher.update(input);
    output.copy_from_slice(&hasher.finalize());
    
    output
}

/// Safe wrapper for RLP encoding.
///
/// Dispatches to C when available (same opt-out via `MEV_DISABLE_FFI=1`).
pub fn rlp_encode(input: &[u8]) -> Vec<u8> {
    #[cfg(has_c_fast_path)]
    if !disable_c_fast_path() {
        let mut out = vec![0u8; input.len().saturating_add(16)];
        let mut written: usize = 0;
        unsafe { mev_rlp_encode_string(input.as_ptr(), input.len(), out.as_mut_ptr(), &mut written); }
        if written > 0 && written <= out.len() {
            out.truncate(written);
            return out;
        }
    }

    // Minimal byte-string RLP encoder used by current call sites.
    if input.len() == 1 && input[0] < 0x80 {
        input.to_vec()
    } else if input.len() < 56 {
        let mut result = vec![0x80 + input.len() as u8];
        result.extend_from_slice(input);
        result
    } else {
        let len_bytes = to_be_bytes(input.len());
        let mut result = vec![0xb7 + len_bytes.len() as u8];
        result.extend_from_slice(&len_bytes);
        result.extend_from_slice(input);
        result
    }
}

fn to_be_bytes(n: usize) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len() - 1);
    bytes[start..].to_vec()
}

/// Returns `true` when the user explicitly opted out of the C fast path
/// by setting `MEV_DISABLE_FFI=1` (or `true`/`yes`/`on`).
///
/// By default, when the C library is compiled in, it is used on every call.
#[cfg(has_c_fast_path)]
fn disable_c_fast_path() -> bool {
    // Cache the result in a static to avoid env var lookup on every call.
    use std::sync::OnceLock;
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        match std::env::var("MEV_DISABLE_FFI") {
            Ok(v) => {
                let normalized = v.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            }
            Err(_) => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keccak256() {
        let input = b"hello";
        let hash = keccak256(input);
        
        // Known hash for "hello"
        let expected = hex::decode(
            "1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        ).unwrap();
        
        assert_eq!(hash.to_vec(), expected);
    }

    #[test]
    fn test_rlp_encode_single_byte() {
        let result = rlp_encode(&[0x42]);
        assert_eq!(result, vec![0x42]);
    }

    #[test]
    fn test_rlp_encode_short_string() {
        let result = rlp_encode(b"dog");
        assert_eq!(result, vec![0x83, b'd', b'o', b'g']);
    }
}
