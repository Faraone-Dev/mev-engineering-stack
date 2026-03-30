// FFI bindings for C hot path - PRODUCTION OPTIMIZED
// Direct calls to C for sub-microsecond operations

use std::ffi::c_void;

// Link to our C library
#[cfg(has_c_fast_path)]
#[link(name = "mev_fast", kind = "static")]
extern "C" {
    // Keccak256
    pub fn mev_keccak256(data: *const u8, len: usize, out: *mut u8);
    pub fn mev_function_selector(signature: *const u8, len: usize, out: *mut u8);
    
    // RLP encoding
    pub fn mev_rlp_encode_string(data: *const u8, len: usize, out: *mut u8) -> usize;
    pub fn mev_rlp_encode_uint256(value: *const u8, out: *mut u8) -> usize;
    pub fn mev_rlp_encode_address(addr: *const u8, out: *mut u8) -> usize;
    
    // Calldata parsing
    pub fn mev_parse_swap(calldata: *const u8, len: usize, info: *mut SwapInfoFFI) -> i32;
    pub fn mev_get_selector(calldata: *const u8, out: *mut u8);
    
    // SIMD utils
    pub fn mev_memcmp_fast(a: *const u8, b: *const u8, len: usize) -> i32;
    pub fn mev_address_eq(a: *const u8, b: *const u8) -> i32;
    pub fn mev_calc_price_impact_batch(
        reserves0: *const u64,
        reserves1: *const u64,
        amount_in: u64,
        outputs: *mut u64,
    );
    pub fn mev_rdtsc() -> u64;
    pub fn mev_prefetch_pool(data: *const c_void);
    
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

/// FFI-compatible swap info
#[repr(C)]
pub struct SwapInfoFFI {
    pub dex_type: u8,        // 1=UniV2, 2=UniV3, 3=Sushi
    pub token_in: [u8; 20],
    pub token_out: [u8; 20],
    pub amount_in: [u8; 32],  // uint256 as bytes
    pub amount_out_min: [u8; 32],
    pub pool_address: [u8; 20],
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

#[derive(Debug, Clone)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
}

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
#[cfg(has_c_fast_path)]
pub mod safe {
    use super::*;
    use ethers::types::{Address, H256, U256};

    pub fn init_pools() -> bool {
        unsafe { mev_pools_init() == 0 }
    }

    #[inline(always)]
    pub fn keccak256_fast(data: &[u8]) -> H256 {
        let mut out = [0u8; 32];
        unsafe { mev_keccak256(data.as_ptr(), data.len(), out.as_mut_ptr()); }
        H256::from(out)
    }

    #[inline(always)]
    pub fn function_selector(signature: &str) -> [u8; 4] {
        let mut out = [0u8; 4];
        unsafe { mev_function_selector(signature.as_ptr(), signature.len(), out.as_mut_ptr()); }
        out
    }

    #[inline(always)]
    pub fn address_eq(a: &Address, b: &Address) -> bool {
        unsafe { mev_address_eq(a.as_bytes().as_ptr(), b.as_bytes().as_ptr()) != 0 }
    }

    #[inline(always)]
    pub fn calc_price_impact_batch(
        reserves0: &[u64; 4], reserves1: &[u64; 4], amount_in: u64,
    ) -> [u64; 4] {
        let mut outputs = [0u64; 4];
        unsafe { mev_calc_price_impact_batch(reserves0.as_ptr(), reserves1.as_ptr(), amount_in, outputs.as_mut_ptr()); }
        outputs
    }

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

    #[inline(always)]
    pub fn rdtsc() -> u64 { unsafe { mev_rdtsc() } }

    #[inline(always)]
    pub fn prefetch<T>(data: &T) {
        unsafe { mev_prefetch_pool(data as *const T as *const c_void); }
    }

    pub fn rlp_encode_address(addr: &Address) -> Vec<u8> {
        let mut out = vec![0u8; 21];
        let len = unsafe { mev_rlp_encode_address(addr.as_bytes().as_ptr(), out.as_mut_ptr()) };
        out.truncate(len);
        out
    }

    pub fn rlp_encode_u256(value: U256) -> Vec<u8> {
        let mut bytes = [0u8; 32];
        value.to_big_endian(&mut bytes);
        let mut out = vec![0u8; 33];
        let len = unsafe { mev_rlp_encode_uint256(bytes.as_ptr(), out.as_mut_ptr()) };
        out.truncate(len);
        out
    }

    // Re-export shared types so `safe::DexType` etc. still resolve
    pub use super::{DexType, SwapInfo};
}

// ══════════════════════════════════════════════════════════════════════
// Pure-Rust fallbacks when the C library is NOT compiled
// ══════════════════════════════════════════════════════════════════════
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

#[cfg(has_c_fast_path)]
unsafe impl Send for OpportunityQueue {}
#[cfg(has_c_fast_path)]
unsafe impl Sync for OpportunityQueue {}

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
    
    #[test]
    fn test_keccak256() {
        // Only run if C lib is available
        if std::env::var("MEV_FAST_LIB").is_ok() {
            let hash = safe::keccak256_fast(b"hello");
            assert!(!hash.is_zero());
        }
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
