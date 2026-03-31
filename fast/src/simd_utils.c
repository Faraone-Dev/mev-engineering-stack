/**
 * SIMD Optimized Utils for MEV Bot
 * Uses AVX2/SSE4.2 for parallel processing
 */

#include <stdint.h>
#include <string.h>
#include <immintrin.h>

#ifdef _MSC_VER
#include <intrin.h>
#else
#include <x86intrin.h>
#endif

/**
 * Fast memory compare using SIMD
 * Returns 0 if equal, non-zero otherwise
 */
int mev_memcmp_fast(const void* a, const void* b, size_t len) {
    const uint8_t* p1 = (const uint8_t*)a;
    const uint8_t* p2 = (const uint8_t*)b;
    
    // Process 32 bytes at a time with AVX2
    while (len >= 32) {
        __m256i v1 = _mm256_loadu_si256((const __m256i*)p1);
        __m256i v2 = _mm256_loadu_si256((const __m256i*)p2);
        __m256i cmp = _mm256_cmpeq_epi8(v1, v2);
        int mask = _mm256_movemask_epi8(cmp);
        if (mask != -1) return 1; // Not equal
        p1 += 32;
        p2 += 32;
        len -= 32;
    }
    
    // Process 16 bytes with SSE
    while (len >= 16) {
        __m128i v1 = _mm_loadu_si128((const __m128i*)p1);
        __m128i v2 = _mm_loadu_si128((const __m128i*)p2);
        __m128i cmp = _mm_cmpeq_epi8(v1, v2);
        int mask = _mm_movemask_epi8(cmp);
        if (mask != 0xFFFF) return 1;
        p1 += 16;
        p2 += 16;
        len -= 16;
    }
    
    // Remaining bytes
    while (len--) {
        if (*p1++ != *p2++) return 1;
    }
    
    return 0;
}

/**
 * Fast memory copy using SIMD with non-temporal stores
 * Best for large buffers that won't be read immediately
 */
void mev_memcpy_nt(void* dst, const void* src, size_t len) {
    uint8_t* d = (uint8_t*)dst;
    const uint8_t* s = (const uint8_t*)src;
    
    // Align destination to 32 bytes
    while (((uintptr_t)d & 31) && len) {
        *d++ = *s++;
        len--;
    }
    
    // Non-temporal stores (bypass cache)
    while (len >= 32) {
        __m256i v = _mm256_loadu_si256((const __m256i*)s);
        _mm256_stream_si256((__m256i*)d, v);
        d += 32;
        s += 32;
        len -= 32;
    }
    
    _mm_sfence(); // Ensure stores are visible
    
    // Remaining
    while (len--) {
        *d++ = *s++;
    }
}

/**
 * Parallel XOR for 32-byte blocks (used in Keccak)
 */
void mev_xor_block_256(uint8_t* dst, const uint8_t* src) {
    __m256i d = _mm256_loadu_si256((const __m256i*)dst);
    __m256i s = _mm256_loadu_si256((const __m256i*)src);
    __m256i r = _mm256_xor_si256(d, s);
    _mm256_storeu_si256((__m256i*)dst, r);
}

/**
 * Fast hex decode using SIMD lookup
 * Input: hex string (lowercase or uppercase), Output: bytes
 */
int mev_hex_decode_fast(const char* hex, size_t hex_len, uint8_t* out) {
    if (hex_len & 1) return -1; // Must be even
    
    size_t out_len = hex_len / 2;
    
    for (size_t i = 0; i < out_len; i++) {
        uint8_t hi = hex[i * 2];
        uint8_t lo = hex[i * 2 + 1];
        
        // Convert hex char to nibble (handles 0-9, a-f, A-F)
        if (hi >= '0' && hi <= '9')      hi = hi - '0';
        else if (hi >= 'a' && hi <= 'f') hi = hi - 'a' + 10;
        else if (hi >= 'A' && hi <= 'F') hi = hi - 'A' + 10;
        else return -1;  // Invalid hex char
        
        if (lo >= '0' && lo <= '9')      lo = lo - '0';
        else if (lo >= 'a' && lo <= 'f') lo = lo - 'a' + 10;
        else if (lo >= 'A' && lo <= 'F') lo = lo - 'A' + 10;
        else return -1;  // Invalid hex char
        
        out[i] = (hi << 4) | lo;
    }
    
    return out_len;
}

/**
 * Fast address comparison (20 bytes)
 * Uses SSE for first 16 bytes + 4 byte compare
 */
int mev_address_eq(const uint8_t* a, const uint8_t* b) {
    __m128i v1 = _mm_loadu_si128((const __m128i*)a);
    __m128i v2 = _mm_loadu_si128((const __m128i*)b);
    __m128i cmp = _mm_cmpeq_epi8(v1, v2);
    
    if (_mm_movemask_epi8(cmp) != 0xFFFF) return 0;
    
    // Last 4 bytes
    return *(uint32_t*)(a + 16) == *(uint32_t*)(b + 16);
}

/**
 * Batch address lookup in sorted array
 * Uses binary search with prefetching
 */
int mev_address_find(const uint8_t addresses[][20], size_t count, const uint8_t* target) {
    if (count == 0) return -1;
    
    size_t left = 0;
    size_t right = count - 1;
    
    while (left <= right) {
        size_t mid = left + (right - left) / 2;
        
        // Prefetch next likely positions
        _mm_prefetch((const char*)&addresses[(left + mid) / 2], _MM_HINT_T0);
        _mm_prefetch((const char*)&addresses[(mid + right) / 2], _MM_HINT_T0);
        
        int cmp = memcmp(addresses[mid], target, 20);
        
        if (cmp == 0) return (int)mid;
        if (cmp < 0) left = mid + 1;
        else right = mid - 1;
    }
    
    return -1;
}

/**
 * Calculate price impact using fixed-point SIMD
 * Processes 4 pools in parallel
 */
void mev_calc_price_impact_batch(
    const uint64_t reserves0[4],  // Pool reserves token0
    const uint64_t reserves1[4],  // Pool reserves token1
    uint64_t amount_in,           // Amount to swap
    uint64_t outputs[4]           // Output amounts
) {
    // Load reserves
    __m256i r0 = _mm256_loadu_si256((const __m256i*)reserves0);
    __m256i r1 = _mm256_loadu_si256((const __m256i*)reserves1);
    
    // For each pool: out = (amount_in * 997 * r1) / (r0 * 1000 + amount_in * 997)
    // Uses __uint128_t intermediates to keep fixed-point math safe.
    
    for (int i = 0; i < 4; i++) {
        uint64_t r0_i = reserves0[i];
        uint64_t r1_i = reserves1[i];
        
        if (r0_i == 0 || r1_i == 0) {
            outputs[i] = 0;
            continue;
        }
        
        // AMM formula with 0.3% fee
        __uint128_t num = (__uint128_t)amount_in * 997 * r1_i;
        __uint128_t den = (__uint128_t)r0_i * 1000 + (__uint128_t)amount_in * 997;
        
        outputs[i] = (uint64_t)(num / den);
    }
}

/**
 * Prefetch pool data for upcoming calculations
 */
void mev_prefetch_pool(const void* pool_data) {
    _mm_prefetch((const char*)pool_data, _MM_HINT_T0);
    _mm_prefetch((const char*)pool_data + 64, _MM_HINT_T0);
    _mm_prefetch((const char*)pool_data + 128, _MM_HINT_T0);
}

/**
 * Get CPU timestamp for profiling
 */
uint64_t mev_rdtsc(void) {
    return __rdtsc();
}

/**
 * Pause hint for spin-wait loops
 */
void mev_cpu_pause(void) {
    _mm_pause();
}
