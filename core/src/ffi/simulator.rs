//! Rust FFI bindings for the C++ AMM simulation and pathfinder kernels.
//!
//! # Safety contract
//! All `unsafe` functions in this module forward to C++ functions compiled
//! from `fast/src/amm_simulator.cpp` and `fast/src/pathfinder.cpp`.
//! The C++ functions never throw, never allocate heap memory, and only write
//! to the `out` pointer they are given.
//!
//! Safe wrappers are provided for every exported symbol; callers should use
//! those rather than the raw `extern "C"` declarations.

// ─── C-compatible structs (must mirror fast/include/amm_simulator.h exactly) ─

/// Pool descriptor — mirrors `AMMPool` in amm_simulator.h
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct AMMPoolC {
    pub token0:        [u8; 20],
    pub token1:        [u8; 20],
    pub pool_addr:     [u8; 20],
    pub reserve0:      u64,
    pub reserve1:      u64,
    pub fee_bps:       u32,
    pub block_updated: u64,
    pub extra:         u64,
    pub is_v3:         u8,
    pub _pad:          [u8; 3],
}

/// Victim swap descriptor — mirrors `VictimSwap` in amm_simulator.h
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct VictimSwapC {
    pub pool_addr:      [u8; 20],
    pub amount_in:      u64,
    pub reserve0_snap:  u64,
    pub reserve1_snap:  u64,
    pub zero_for_one:   u8,
    pub _pad:           [u8; 7],
}

/// Sandwich simulation output — mirrors `SandwichResult` in amm_simulator.h
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct SandwichResultC {
    pub frontrun_amount: u64,
    pub backrun_amount:  u64,
    pub gross_profit:    i64,
    pub net_profit:      i64,
    pub valid:           u8,
    pub _pad:            [u8; 7],
}

/// Per-hop pool info — mirrors `HopPool` in pathfinder.h
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct HopPoolC {
    pub pool_addr: [u8; 20],
    pub token_in:  [u8; 20],
    pub token_out: [u8; 20],
    pub fee_bps:   u32,
    pub is_v3:     u8,
    pub _pad:      [u8; 3],
}

/// A multi-hop path — mirrors `Path` in pathfinder.h
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PathC {
    pub hops:   [HopPoolC; 4],
    pub n_hops: u32,
    pub _pad:   [u8; 4],
}

impl Default for PathC {
    fn default() -> Self {
        Self {
            hops:   [HopPoolC::default(); 4],
            n_hops: 0,
            _pad:   [0; 4],
        }
    }
}

/// Pathfinder result — mirrors `PathfinderResult` in pathfinder.h
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PathfinderResultC {
    pub best_path:      PathC,
    pub optimal_amount: u64,
    pub gross_profit:   i64,
    pub valid:          u8,
    pub _pad:           [u8; 7],
}

// ─── Raw extern "C" declarations ─────────────────────────────────────────────

#[cfg(has_c_fast_path)]
#[link(name = "mev_fast_cpp", kind = "static")]
extern "C" {
    // AMM simulator
    fn amm_v2_amount_out(
        reserve_in:  u64,
        reserve_out: u64,
        fee_bps:     u32,
        amount_in:   u64,
    ) -> u64;

    fn amm_v2_amount_in(
        reserve_in:  u64,
        reserve_out: u64,
        fee_bps:     u32,
        amount_out:  u64,
    ) -> u64;

    fn amm_v3_amount_out(
        liquidity:       u64,
        sqrt_price_x64:  u64,
        zero_for_one:    u8,
        fee_bps:         u32,
        amount_in:       u64,
    ) -> u64;

    fn amm_find_optimal_frontrun(
        pool:   *const AMMPoolC,
        victim: *const VictimSwapC,
        out:    *mut SandwichResultC,
    ) -> i32;

    fn amm_batch_find_optimal(
        pools:   *const AMMPoolC,
        victims: *const VictimSwapC,
        results: *mut SandwichResultC,
        n:       u32,
    );

    // Pathfinder — PoolGraph is an opaque large struct; callers must allocate it
    #[allow(dead_code)]
    fn pathfinder_token_fp(addr20: *const u8) -> u64;

    #[allow(dead_code)]
    fn pathfinder_find_best(
        graph:        *const u8,  // *const PoolGraph (opaque)
        token_in_fp:  u64,
        token_out_fp: u64,
        amount_hint:  u64,
        out:          *mut PathfinderResultC,
    ) -> i32;

    #[allow(dead_code)]
    fn pathfinder_graph_upsert(graph: *mut u8, pool: *const AMMPoolC) -> i32;
    #[allow(dead_code)]
    fn pathfinder_graph_clear(graph: *mut u8);
    #[allow(dead_code)]
    fn pathfinder_graph_size(graph: *const u8) -> u32;
}

// ─── Safe wrappers ────────────────────────────────────────────────────────────

/// V2 constant-product getAmountOut.
///
/// Returns `None` if either reserve is zero, amount_in is zero, or the result
/// overflows u64.  Uses __uint128_t internally to handle mainnet reserve scale.
#[inline]
pub fn v2_amount_out(reserve_in: u64, reserve_out: u64, fee_bps: u32, amount_in: u64) -> Option<u64> {
    #[cfg(has_c_fast_path)]
    {
        let out = unsafe { amm_v2_amount_out(reserve_in, reserve_out, fee_bps, amount_in) };
        return if out == 0 { None } else { Some(out) };
    }
    #[cfg(not(has_c_fast_path))]
    v2_amount_out_rust(reserve_in, reserve_out, fee_bps, amount_in)
}

/// V2 constant-product getAmountIn (solve for exact output).
#[inline]
pub fn v2_amount_in(reserve_in: u64, reserve_out: u64, fee_bps: u32, amount_out: u64) -> Option<u64> {
    #[cfg(has_c_fast_path)]
    {
        let needed = unsafe { amm_v2_amount_in(reserve_in, reserve_out, fee_bps, amount_out) };
        return if needed == 0 { None } else { Some(needed) };
    }
    #[cfg(not(has_c_fast_path))]
    v2_amount_in_rust(reserve_in, reserve_out, fee_bps, amount_out)
}

/// V3 single-tick approximate getAmountOut.
///
/// Accurate to ~1-2% for swaps within a single tick.  Real tick-crossing
/// requires the full TickMath library; this is intentionally lightweight for
/// simulation purposes.
#[inline]
pub fn v3_amount_out(liquidity: u64, sqrt_price_x64: u64, zero_for_one: bool, fee_bps: u32, amount_in: u64) -> Option<u64> {
    #[cfg(has_c_fast_path)]
    {
        let out = unsafe {
            amm_v3_amount_out(liquidity, sqrt_price_x64, zero_for_one as u8, fee_bps, amount_in)
        };
        return if out == 0 { None } else { Some(out) };
    }
    #[cfg(not(has_c_fast_path))]
    { let _ = (liquidity, sqrt_price_x64, zero_for_one, fee_bps, amount_in); None }
}

/// Find optimal sandwich parameters for a single pool+victim pair.
///
/// Returns `Some(SandwichResultC)` when a profitable result is found,
/// `None` if the simulation produced no profitable opportunity.
pub fn find_optimal_frontrun(pool: &AMMPoolC, victim: &VictimSwapC) -> Option<SandwichResultC> {
    #[cfg(has_c_fast_path)]
    {
        let mut result = SandwichResultC::default();
        let ok = unsafe {
            amm_find_optimal_frontrun(
                pool   as *const _,
                victim as *const _,
                &mut result,
            )
        };
        return if ok == 1 { Some(result) } else { None };
    }
    #[cfg(not(has_c_fast_path))]
    { let _ = (pool, victim); None }
}

/// Batch version of `find_optimal_frontrun`.
///
/// Processes `pools.len()` pairs and writes results into `results`.
/// `pools`, `victims`, and `results` must all have the same length.
///
/// # Panics
/// Panics if `pools.len() != victims.len()` or `pools.len() != results.len()`.
pub fn batch_find_optimal(
    pools:   &[AMMPoolC],
    victims: &[VictimSwapC],
    results: &mut [SandwichResultC],
) {
    assert_eq!(pools.len(), victims.len());
    assert_eq!(pools.len(), results.len());

    #[cfg(has_c_fast_path)]
    unsafe {
        amm_batch_find_optimal(
            pools.as_ptr(),
            victims.as_ptr(),
            results.as_mut_ptr(),
            pools.len() as u32,
        );
    }
    #[cfg(not(has_c_fast_path))]
    {
        // Rust-only fallback: call the safe single-pair function repeatedly
        for i in 0..pools.len() {
            results[i] = find_optimal_frontrun(&pools[i], &victims[i])
                .unwrap_or_default();
        }
    }
}

/// Compute a 64-bit fingerprint of a 20-byte EVM address.
///
/// Use this to convert `ethers::types::Address` bytes to a `u64` before
/// passing to `pathfinder_find_best`.
#[inline]
pub fn token_fingerprint(addr: &[u8; 20]) -> u64 {
    #[cfg(has_c_fast_path)]
    {
        return unsafe { pathfinder_token_fp(addr.as_ptr()) };
    }
    #[cfg(not(has_c_fast_path))]
    fnv1a_64(addr)
}

// ─── Pure-Rust fallbacks (used when C++ compilation is unavailable) ──────────

/// Pure-Rust V2 getAmountOut — identical math to the C++ version.
#[cfg(not(has_c_fast_path))]
#[inline]
pub fn v2_amount_out_rust(
    reserve_in: u64, reserve_out: u64, fee_bps: u32, amount_in: u64,
) -> Option<u64> {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 { return None; }
    let fc = 10_000u128 - fee_bps as u128 / 100;
    let ain = amount_in as u128 * fc;
    let num = ain * reserve_out as u128;
    let den = reserve_in as u128 * 10_000 + ain;
    if den == 0 { return None; }
    let out = num / den;
    if out > u64::MAX as u128 { return None; }
    Some(out as u64)
}

#[cfg(not(has_c_fast_path))]
#[inline]
pub fn v2_amount_in_rust(
    reserve_in: u64, reserve_out: u64, fee_bps: u32, amount_out: u64,
) -> Option<u64> {
    if reserve_in == 0 || reserve_out == 0 || amount_out >= reserve_out { return None; }
    let fc = 10_000u128 - fee_bps as u128 / 100;
    let num = reserve_in as u128 * amount_out as u128 * 10_000;
    let den = (reserve_out as u128 - amount_out as u128) * fc;
    if den == 0 { return None; }
    let ain = num / den + 1;
    if ain > u64::MAX as u128 { return None; }
    Some(ain as u64)
}

#[allow(dead_code)]
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_amount_out_basic() {
        // 1 ETH in, 1000 ETH / 1_000_000 USDC pool, 0.3% fee
        let r0: u64 = 1_000 * 10u64.pow(18);
        let r1: u64 = 1_000_000 * 10u64.pow(6);
        let out = v2_amount_out(r0, r1, 3000, 10u64.pow(18));
        assert!(out.is_some());
        // ~997 USDC out of 1_000_000 pool for 1 ETH in 1000 ETH pool
        let out_u = out.unwrap();
        assert!(out_u > 900 * 10u64.pow(6), "got {}", out_u);
        assert!(out_u < 1_100 * 10u64.pow(6), "got {}", out_u);
    }

    #[test]
    fn v2_amount_out_mainnet_scale() {
        // At mainnet reserve scale (1e20), intermediates overflow u64/u128
        // The C++ impl uses __uint128_t so this should not return 0
        let r0: u64 = 100_000 * 10u64.pow(18);  // 1e23 — large pool
        let r1: u64 = u64::MAX / 2;              // saturate
        let out = v2_amount_out(r0, r1, 3000, 10u64.pow(18));
        // Should not panic and should return a reasonable value or None
        let _ = out;
    }

    #[test]
    fn token_fingerprint_stable() {
        let addr = [1u8; 20];
        let fp1 = token_fingerprint(&addr);
        let fp2 = token_fingerprint(&addr);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn token_fingerprint_distinct() {
        let a = [0u8; 20];
        let mut b = [0u8; 20];
        b[19] = 1;
        assert_ne!(token_fingerprint(&a), token_fingerprint(&b));
    }
}
