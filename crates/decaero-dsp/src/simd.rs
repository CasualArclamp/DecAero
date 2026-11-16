// made by CasualArclamp
//! The inner loops, with AVX2 and AVX-512 copies picked at run time.
//!
//! The FIR is DecSAT's (`decsat-dsp/src/dot.rs`, same author, GPL-3): a
//! filter that does not decimate works out a block of neighbouring outputs
//! together, so each tap is loaded once for all of them and the sums stay in
//! registers. The oscillator multiply of the down-converter is done the same
//! way here.
//!
//! Rust note: a release build targets plain x86-64 (SSE2) so that the one
//! exe runs anywhere. `#[target_feature]` compiles further copies of the
//! same loop for wider vectors, and `is_x86_feature_detected!` (a cached flag
//! test) picks one. They are the plain loop recompiled, with no
//! fused multiply-add, so every copy gives bit-identical results.

use num_complex::Complex32;

/// Outputs [`fir_cr`] works out together on SSE2, AVX2 and AVX-512 (DecSAT's
/// measured choices: 16 complex sums are 8 SSE registers, and so on up).
const BLOCK: usize = 16;
#[cfg(target_arch = "x86_64")]
const BLOCK_AVX2: usize = 32;
#[cfg(target_arch = "x86_64")]
const BLOCK_AVX512: usize = 64;

#[cfg(target_arch = "x86_64")]
fn has_avx512() -> bool {
    std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw")
        && std::arch::is_x86_feature_detected!("avx512vl")
        && std::arch::is_x86_feature_detected!("avx512dq")
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

/// Name of the widest vector unit in use on this CPU, for display.
pub fn vector_level() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx512() {
            "AVX-512"
        } else if has_avx2() {
            "AVX2"
        } else {
            "SSE2"
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        "portable"
    }
}

/// Every output of a filter with taps `h_rev` (reversed) that fits in `x`,
/// appended to `out`: `out[j] = sum over k of x[j + k] * h_rev[k]`.
pub fn fir_cr(x: &[Complex32], h_rev: &[f32], out: &mut Vec<Complex32>) {
    let l = h_rev.len();
    if l == 0 || x.len() < l {
        return;
    }
    let n_out = x.len() + 1 - l;
    #[cfg(target_arch = "x86_64")]
    if n_out >= BLOCK {
        if has_avx512() {
            // SAFETY: the CPU has the features the function was compiled for.
            unsafe { fir_avx512(x, h_rev, n_out, out) };
            return;
        }
        if has_avx2() {
            // SAFETY: as above.
            unsafe { fir_avx2(x, h_rev, n_out, out) };
            return;
        }
    }
    fir_body::<BLOCK>(x, h_rev, n_out, out);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fir_avx2(x: &[Complex32], h_rev: &[f32], n_out: usize, out: &mut Vec<Complex32>) {
    fir_body::<BLOCK_AVX2>(x, h_rev, n_out, out);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512dq")]
unsafe fn fir_avx512(x: &[Complex32], h_rev: &[f32], n_out: usize, out: &mut Vec<Complex32>) {
    fir_body::<BLOCK_AVX512>(x, h_rev, n_out, out);
}

/// `WIDE` outputs at a time, then [`BLOCK`], then singly. `WIDE` is a
/// multiple of [`BLOCK`], so the same last few outputs reach [`dot_cr`] on
/// every CPU and the results do not depend on which copy ran.
#[inline(always)]
fn fir_body<const WIDE: usize>(
    x: &[Complex32],
    h_rev: &[f32],
    n_out: usize,
    out: &mut Vec<Complex32>,
) {
    out.reserve(n_out);
    let mut j = fir_blocks::<WIDE>(x, h_rev, n_out, 0, out);
    if WIDE != BLOCK {
        j = fir_blocks::<BLOCK>(x, h_rev, n_out, j, out);
    }
    let l = h_rev.len();
    for j in j..n_out {
        out.push(dot_cr(&x[j..j + l], h_rev));
    }
}

#[inline(always)]
fn fir_blocks<const B: usize>(
    x: &[Complex32],
    h_rev: &[f32],
    n_out: usize,
    mut j: usize,
    out: &mut Vec<Complex32>,
) -> usize {
    while j + B <= n_out {
        let mut acc = [Complex32::new(0.0, 0.0); B];
        for (k, &h) in h_rev.iter().enumerate() {
            // Rust note: `try_into` turns the slice into a `&[Complex32; B]`,
            // so the loop below has a constant trip count and no bounds
            // checks: it unrolls into vector multiply-adds.
            let xs: &[Complex32; B] = x[j + k..j + k + B].try_into().unwrap();
            for (a, &s) in acc.iter_mut().zip(xs) {
                *a += s * h;
            }
        }
        out.extend_from_slice(&acc);
        j += B;
    }
    j
}

/// `sum of x[k] * h[k]` for complex `x` and real `h` of equal length, with
/// eight partial sums so the compiler can vectorise it.
#[inline]
pub fn dot_cr(x: &[Complex32], h: &[f32]) -> Complex32 {
    let n = x.len().min(h.len());
    let (x, h) = (&x[..n], &h[..n]);
    let mut re = [0.0f32; 8];
    let mut im = [0.0f32; 8];
    let (xc, xr) = x.as_chunks::<8>();
    let (hc, hr) = h.as_chunks::<8>();
    for (xs, hs) in xc.iter().zip(hc) {
        for k in 0..8 {
            re[k] += xs[k].re * hs[k];
            im[k] += xs[k].im * hs[k];
        }
    }
    let mut acc = Complex32::new(re.iter().sum(), im.iter().sum());
    for (s, &t) in xr.iter().zip(hr) {
        acc += s * t;
    }
    acc
}

/// Samples the oscillator multiply handles per call (the length of its table).
pub const MIX: usize = 64;

/// The down-converter's oscillator: `out[k] = x[k] * base * table[k]`, where
/// `table[k]` is the oscillator's turn after `k` samples and `base` its
/// position at the first. All three slices are [`MIX`] long.
pub fn mix(
    x: &[Complex32; MIX],
    base: Complex32,
    table: &[Complex32; MIX],
    out: &mut [Complex32; MIX],
) {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx512() {
            // SAFETY: the CPU has the features the function was compiled for.
            unsafe { mix_avx512(x, base, table, out) };
            return;
        }
        if has_avx2() {
            // SAFETY: as above.
            unsafe { mix_avx2(x, base, table, out) };
            return;
        }
    }
    mix_body(x, base, table, out);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mix_avx2(
    x: &[Complex32; MIX],
    base: Complex32,
    table: &[Complex32; MIX],
    out: &mut [Complex32; MIX],
) {
    mix_body(x, base, table, out);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512dq")]
unsafe fn mix_avx512(
    x: &[Complex32; MIX],
    base: Complex32,
    table: &[Complex32; MIX],
    out: &mut [Complex32; MIX],
) {
    mix_body(x, base, table, out);
}

#[inline(always)]
fn mix_body(
    x: &[Complex32; MIX],
    base: Complex32,
    table: &[Complex32; MIX],
    out: &mut [Complex32; MIX],
) {
    for k in 0..MIX {
        // Written out in real arithmetic so the loop vectorises as four
        // multiplies and two adds per sample, twice.
        let (tr, ti) = (
            base.re * table[k].re - base.im * table[k].im,
            base.re * table[k].im + base.im * table[k].re,
        );
        out[k] = Complex32::new(x[k].re * tr - x[k].im * ti, x[k].re * ti + x[k].im * tr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize) -> Vec<Complex32> {
        let mut seed = 3u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 8_388_608.0 - 1.0
        };
        (0..n).map(|_| Complex32::new(next(), next())).collect()
    }

    #[test]
    fn fir_matches_a_dot_product_per_window() {
        let x = signal(700);
        let h: Vec<f32> = (0..129)
            .map(|i| ((i * 7 % 13) as f32 - 6.0) / 10.0)
            .collect();
        let mut out = Vec::new();
        fir_cr(&x, &h, &mut out);
        assert_eq!(out.len(), 700 - 128);
        for (j, o) in out.iter().enumerate() {
            let want = dot_cr(&x[j..j + 129], &h);
            assert!((o - want).norm() < 1e-4, "output {j}");
        }
    }

    #[test]
    fn every_vector_width_gives_identical_bits() {
        let x = signal(900);
        let h: Vec<f32> = (0..73).map(|i| (i as f32 * 0.37).sin()).collect();
        let n_out = x.len() + 1 - h.len();
        let mut plain = Vec::new();
        fir_body::<BLOCK>(&x, &h, n_out, &mut plain);
        let mut picked = Vec::new();
        fir_cr(&x, &h, &mut picked);
        assert_eq!(plain, picked);

        let block: &[Complex32; MIX] = x[..MIX].try_into().unwrap();
        let table: [Complex32; MIX] =
            std::array::from_fn(|k| Complex32::from_polar(1.0, 0.01 * k as f32));
        let base = Complex32::from_polar(1.0, 0.7);
        let (mut a, mut b) = ([Complex32::default(); MIX], [Complex32::default(); MIX]);
        mix_body(block, base, &table, &mut a);
        mix(block, base, &table, &mut b);
        assert_eq!(a, b);
        for k in 0..MIX {
            assert!((a[k] - block[k] * base * table[k]).norm() < 1e-5);
        }
    }
}
