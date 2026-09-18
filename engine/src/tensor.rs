//! The small amount of linear algebra the transliteration model needs.
//!
//! Deliberately not a matrix library. The model is 11.8M parameters decoding one word at a time:
//! the largest single product is 1024x256, and the whole beam search is a few hundred MFLOPs. A
//! BLAS would add a large native dependency, a build-system problem for the 32-bit target, and
//! CPU-dispatch bugs on users' machines, to accelerate something that is already fast enough in
//! plain loops the compiler can vectorize.
//!
//! **Accumulator choice.** Matrix products accumulate in f32, because that is what NumPy's f32
//! gemm does and because f32 lets the compiler use the full SIMD width -- the difference matters
//! here, where the FFN dominates. The reductions that are numerically delicate and cheap --
//! layer-norm's mean and variance, softmax's sum, log-softmax's logsumexp -- accumulate in f64
//! instead. Those are O(n) over 256 or 1024 elements, so the cost is nothing, and doing them in
//! f64 keeps this closer to the exact value than NumPy's own pairwise f32 summation, rather than
//! drifting away from it in a different direction.
//!
//! No unsafe code and no manual SIMD: the loops below are written in the order that lets LLVM
//! vectorize them, which measured as fast as hand-written intrinsics would be at these sizes.

/// `out[m, n] = a[m, k] @ w[k, n] + bias[n]`, all row-major.
///
/// Loop order is i-p-j, not the textbook i-j-p: it walks one row of `w` contiguously in the inner
/// loop and accumulates into one row of `out`, so both are sequential and the inner loop vectorizes.
/// i-j-p would stride down a column of `w` and defeat the prefetcher.
///
/// The `p` loop is unrolled four ways. The inner loop is a read-modify-write of `out`, so
/// consecutive `p` iterations form a dependency chain on the same accumulator and the FMA units sit
/// idle waiting for it; four independent products per pass keeps them fed. This is worth more than
/// it looks -- it roughly halved the full-path latency.
#[inline(always)]
fn matmul_kernel(a: &[f32], m: usize, k: usize, w: &[f32], n: usize, bias: Option<&[f32]>, out: &mut [f32]) {
    for i in 0..m {
        let out_row = &mut out[i * n..(i + 1) * n];
        match bias {
            Some(b) => out_row.copy_from_slice(&b[..n]),
            None => out_row.fill(0.0),
        }
        let a_row = &a[i * k..(i + 1) * k];

        let mut p = 0;
        while p + 4 <= k {
            let (a0, a1, a2, a3) = (a_row[p], a_row[p + 1], a_row[p + 2], a_row[p + 3]);
            if a0 == 0.0 && a1 == 0.0 && a2 == 0.0 && a3 == 0.0 {
                p += 4;
                continue;
            }
            let w0 = &w[p * n..(p + 1) * n];
            let w1 = &w[(p + 1) * n..(p + 2) * n];
            let w2 = &w[(p + 2) * n..(p + 3) * n];
            let w3 = &w[(p + 3) * n..(p + 4) * n];
            for j in 0..n {
                out_row[j] += a0 * w0[j] + a1 * w1[j] + a2 * w2[j] + a3 * w3[j];
            }
            p += 4;
        }
        while p < k {
            let av = a_row[p];
            if av != 0.0 {
                let w_row = &w[p * n..(p + 1) * n];
                for j in 0..n {
                    out_row[j] += av * w_row[j];
                }
            }
            p += 1;
        }
    }
}

/// The same kernel compiled for AVX2 with FMA.
///
/// No intrinsics: the portable kernel is written so the compiler vectorizes it, and this simply
/// compiles it again with wider registers and fused multiply-add available. The crate is built for
/// baseline x86-64, which means SSE2, because a keyboard is installed on whatever machine someone
/// has and an illegal-instruction crash inside Word is not an acceptable failure mode. Dispatching
/// at runtime gets the width on the machines that have it and stays correct on the ones that
/// do not.
///
/// # Safety
/// Only called after `is_x86_feature_detected!` has confirmed both features.
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn matmul_kernel_avx2(a: &[f32], m: usize, k: usize, w: &[f32], n: usize, bias: Option<&[f32]>, out: &mut [f32]) {
    matmul_kernel(a, m, k, w, n, bias, out)
}

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
fn have_avx2() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    // 0 = not yet probed, 1 = yes, 2 = no. Probing is cheap but this is on the hot path.
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let yes = is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma");
            STATE.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
            yes
        }
    }
}

pub fn matmul_bias(a: &[f32], m: usize, k: usize, w: &[f32], n: usize, bias: Option<&[f32]>, out: &mut [f32]) {
    debug_assert_eq!(a.len(), m * k);
    debug_assert_eq!(w.len(), k * n);
    debug_assert_eq!(out.len(), m * n);

    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    if have_avx2() {
        // SAFETY: guarded by the runtime feature check above.
        unsafe { matmul_kernel_avx2(a, m, k, w, n, bias, out) };
        return;
    }
    matmul_kernel(a, m, k, w, n, bias, out)
}

/// Layer norm over the last dimension, in place of `(x - mean) / sqrt(var + eps) * g + b`.
///
/// eps is 1e-5, matching the Python default and fairseq's.
pub fn layer_norm(x: &[f32], rows: usize, dim: usize, g: &[f32], b: &[f32], out: &mut [f32]) {
    debug_assert_eq!(x.len(), rows * dim);
    debug_assert_eq!(g.len(), dim);
    debug_assert_eq!(b.len(), dim);
    const EPS: f64 = 1e-5;

    for r in 0..rows {
        let row = &x[r * dim..(r + 1) * dim];
        let mut sum = 0.0f64;
        for &v in row {
            sum += v as f64;
        }
        let mean = sum / dim as f64;
        let mut var = 0.0f64;
        for &v in row {
            let d = v as f64 - mean;
            var += d * d;
        }
        var /= dim as f64;
        let inv = 1.0 / (var + EPS).sqrt();
        let dst = &mut out[r * dim..(r + 1) * dim];
        for j in 0..dim {
            dst[j] = (((row[j] as f64 - mean) * inv) as f32) * g[j] + b[j];
        }
    }
}

const GELU_C: f32 = 0.797_884_6; // sqrt(2/pi), as f32

/// Hendrycks & Gimpel tanh approximation of GELU, matching `xlit_np.gelu`.
///
/// fairseq's "gelu" is the exact erf form; the Python engine deliberately uses this approximation,
/// which is within 2.3e-4 of it and measured as having no effect on any evaluation set. Using the
/// exact form here would be a *divergence from the reference*, not an improvement.
pub fn gelu_inplace(x: &mut [f32]) {
    for v in x.iter_mut() {
        let t = *v;
        *v = 0.5 * t * (1.0 + (GELU_C * (t + 0.044715 * t * t * t)).tanh());
    }
}

pub fn relu_inplace(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = v.max(0.0);
    }
}

/// Multi-head scaled dot-product attention over head-interleaved rows.
///
/// `q`, `k`, `v` are row-major `(T, dim)` where each row holds every head end to end, which is the
/// layout the fused q/k/v projection already produces. So head `h` of row `t` lives at
/// `t*dim + h*head_dim`, and no transpose is needed anywhere -- NumPy's `_split`/`_merge` pair
/// exists only because NumPy wants the head axis in front to batch its matmuls.
///
/// Queries are expected to be pre-scaled (the builder folds the 1/sqrt(head_dim) into the q columns
/// of the fused projection, and cross-attention scales explicitly), matching the Python.
///
/// `causal` applies fairseq's mask: position i may not attend beyond `tk - tq + i`. With tq == tk
/// that is the ordinary lower triangle; with tq == 1 (an incremental decode step) nothing is
/// masked, which is why the step path passes false.
// `scratch` is a `&mut Vec` on purpose: attention resizes it to the key length, which varies per
// call as the decoder's cache grows, and the caller reuses one buffer across every layer and step.
// A slice cannot grow, and allocating per call would put a malloc inside the innermost loop.
#[allow(clippy::too_many_arguments, clippy::ptr_arg)]
pub fn attention(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    tq: usize,
    tk: usize,
    dim: usize,
    heads: usize,
    causal: bool,
    out: &mut [f32],
    scratch: &mut Vec<f32>,
) {
    let head_dim = dim / heads;
    debug_assert_eq!(q.len(), tq * dim);
    debug_assert_eq!(k.len(), tk * dim);
    debug_assert_eq!(v.len(), tk * dim);
    debug_assert_eq!(out.len(), tq * dim);

    scratch.clear();
    scratch.resize(tk, 0.0);

    for h in 0..heads {
        let base = h * head_dim;
        for i in 0..tq {
            let qrow = &q[i * dim + base..i * dim + base + head_dim];
            // Python masks with -1e30 rather than -inf, then subtracts the row max. Reproduced
            // exactly: a masked score that is merely very negative, not infinite, keeps the
            // arithmetic finite even if an entire row were masked.
            let limit = if causal { tk - tq + i + 1 } else { tk };
            let mut max = f32::NEG_INFINITY;
            for j in 0..tk {
                let s = if j < limit {
                    let krow = &k[j * dim + base..j * dim + base + head_dim];
                    let mut acc = 0.0f32;
                    for d in 0..head_dim {
                        acc += qrow[d] * krow[d];
                    }
                    acc
                } else {
                    -1e30
                };
                scratch[j] = s;
                if s > max {
                    max = s;
                }
            }
            let mut total = 0.0f64;
            for s in scratch.iter_mut() {
                let e = (*s - max).exp();
                *s = e;
                total += e as f64;
            }
            let inv = (1.0 / total) as f32;

            let orow = &mut out[i * dim + base..i * dim + base + head_dim];
            orow.fill(0.0);
            for j in 0..tk {
                let p = scratch[j] * inv;
                if p == 0.0 {
                    continue;
                }
                let vrow = &v[j * dim + base..j * dim + base + head_dim];
                for d in 0..head_dim {
                    orow[d] += p * vrow[d];
                }
            }
        }
    }
}

/// `x - max - log(sum(exp(x - max)))`, in place. The logsumexp accumulates in f64.
pub fn log_softmax_inplace(x: &mut [f32]) {
    let mut max = f32::NEG_INFINITY;
    for &v in x.iter() {
        if v > max {
            max = v;
        }
    }
    if !max.is_finite() {
        // Every entry is -inf (or the row is empty): there is no distribution to form, and leaving
        // the row alone keeps the -inf that callers already treat as "impossible".
        return;
    }
    let mut total = 0.0f64;
    for &v in x.iter() {
        total += ((v - max) as f64).exp();
    }
    let log_total = total.ln() as f32;
    for v in x.iter_mut() {
        *v = *v - max - log_total;
    }
}

pub fn add_inplace(x: &mut [f32], y: &[f32]) {
    for (a, b) in x.iter_mut().zip(y.iter()) {
        *a += b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs().max(b.abs()))
    }

    #[test]
    fn matmul_matches_a_naive_reference() {
        let (m, k, n) = (3, 4, 5);
        let a: Vec<f32> = (0..m * k).map(|i| (i as f32 * 0.37).sin()).collect();
        let w: Vec<f32> = (0..k * n).map(|i| (i as f32 * 0.11).cos()).collect();
        let bias: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();
        let mut got = vec![0.0; m * n];
        matmul_bias(&a, m, k, &w, n, Some(&bias), &mut got);
        for i in 0..m {
            for j in 0..n {
                let mut want = bias[j];
                for p in 0..k {
                    want += a[i * k + p] * w[p * n + j];
                }
                assert!(close(got[i * n + j], want, 1e-5), "({i},{j})");
            }
        }
    }

    #[test]
    fn layer_norm_produces_zero_mean_unit_variance() {
        let dim = 256;
        let x: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.7).sin() * 3.0 + 1.0).collect();
        let g = vec![1.0f32; dim];
        let b = vec![0.0f32; dim];
        let mut out = vec![0.0; dim];
        layer_norm(&x, 1, dim, &g, &b, &mut out);
        let mean: f64 = out.iter().map(|&v| v as f64).sum::<f64>() / dim as f64;
        let var: f64 = out.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / dim as f64;
        assert!(mean.abs() < 1e-5, "mean {mean}");
        assert!((var - 1.0).abs() < 1e-3, "var {var}");
    }

    #[test]
    fn log_softmax_sums_to_one_in_probability_space() {
        let mut x: Vec<f32> = vec![1.0, 2.0, 3.0, -50.0, 0.5];
        log_softmax_inplace(&mut x);
        let total: f64 = x.iter().map(|&v| (v as f64).exp()).sum();
        assert!((total - 1.0).abs() < 1e-6, "total {total}");
    }

    #[test]
    fn causal_attention_cannot_see_the_future() {
        // With tq == tk, row 0 must depend only on key 0. Give key 0 and key 1 very different
        // values: if the mask leaked, row 0 would move.
        let (t, dim, heads) = (2, 4, 1);
        let q = vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let k = vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let v = vec![5.0, 5.0, 5.0, 5.0, -100.0, -100.0, -100.0, -100.0];
        let mut out = vec![0.0; t * dim];
        let mut scratch = Vec::new();
        attention(&q, &k, &v, t, t, dim, heads, true, &mut out, &mut scratch);
        assert!(close(out[0], 5.0, 1e-6), "row 0 leaked the future: {}", out[0]);
        // Row 1 sees both keys equally, so it averages them.
        assert!(close(out[dim], -47.5, 1e-4), "row 1 = {}", out[dim]);
    }

    #[test]
    fn uncausal_attention_averages_everything() {
        let (t, dim, heads) = (2, 4, 1);
        let q = vec![0.0; 8];
        let k = vec![0.0; 8];
        let v = vec![1.0, 1.0, 1.0, 1.0, 3.0, 3.0, 3.0, 3.0];
        let mut out = vec![0.0; t * dim];
        let mut scratch = Vec::new();
        attention(&q, &k, &v, t, t, dim, heads, false, &mut out, &mut scratch);
        // All scores equal -> uniform weights -> the mean of v.
        assert!(close(out[0], 2.0, 1e-6), "{}", out[0]);
    }
}
