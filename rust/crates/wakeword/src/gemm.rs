//! Small f32 matrix multiply for the convolution / dense layers: a 6x16
//! register-blocked kernel, compiled twice (AVX2 + FMA picked at run time on
//! x86-64, plus a portable fallback).

/// Rows of A per micro-tile.
pub const MR: usize = 6;
/// Columns of B per micro-tile.
pub const NR: usize = 16;

pub fn round_up(x: usize, m: usize) -> usize {
    x.div_ceil(m) * m
}

/// `c[mp x np] = a[mp x k] * b[k x np]` (row-major), with `mp % MR == 0`
/// and `np % NR == 0` (callers zero-pad).
pub fn gemm(a: &[f32], b: &[f32], c: &mut [f32], mp: usize, k: usize, np: usize) {
    assert!(mp.is_multiple_of(MR) && np.is_multiple_of(NR));
    assert!(a.len() >= mp * k && b.len() >= k * np && c.len() >= mp * np);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
    {
        // SAFETY: the CPU supports AVX2 and FMA (checked just above).
        unsafe { gemm_avx2(a, b, c, mp, k, np) };
        return;
    }
    kernel::<{ cfg!(target_feature = "fma") }>(a, b, c, mp, k, np);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
fn gemm_avx2(a: &[f32], b: &[f32], c: &mut [f32], mp: usize, k: usize, np: usize) {
    kernel::<true>(a, b, c, mp, k, np);
}

#[inline(always)]
fn kernel<const FMA: bool>(a: &[f32], b: &[f32], c: &mut [f32], mp: usize, k: usize, np: usize) {
    for j in (0..np).step_by(NR) {
        for i in (0..mp).step_by(MR) {
            let rows: [&[f32]; MR] = std::array::from_fn(|r| &a[(i + r) * k..(i + r + 1) * k]);
            let mut acc = [[0.0f32; NR]; MR];
            for kk in 0..k {
                let off = kk * np + j;
                let bp: &[f32; NR] = b[off..off + NR].try_into().expect("NR slice");
                for r in 0..MR {
                    let av = rows[r][kk];
                    for t in 0..NR {
                        acc[r][t] = if FMA {
                            av.mul_add(bp[t], acc[r][t])
                        } else {
                            acc[r][t] + av * bp[t]
                        };
                    }
                }
            }
            for (r, row) in acc.iter().enumerate() {
                let off = (i + r) * np + j;
                c[off..off + NR].copy_from_slice(row);
            }
        }
    }
}

/// A constant left operand (`m x k`), rows zero-padded to a multiple of MR.
#[derive(Clone, Debug)]
pub struct PackedA {
    pub k: usize,
    pub mp: usize,
    pub data: Vec<f32>,
}

impl PackedA {
    pub fn new(a: &[f32], m: usize, k: usize) -> Self {
        let mp = round_up(m.max(1), MR);
        let mut data = vec![0.0; mp * k];
        data[..m * k].copy_from_slice(&a[..m * k]);
        Self { k, mp, data }
    }
}

/// A constant right operand (`k x n`), columns zero-padded to a multiple of
/// NR.
#[derive(Clone, Debug)]
pub struct PackedB {
    pub k: usize,
    pub n: usize,
    pub np: usize,
    pub data: Vec<f32>,
}

impl PackedB {
    pub fn new(b: &[f32], k: usize, n: usize) -> Self {
        let np = round_up(n.max(1), NR);
        let mut data = vec![0.0; k * np];
        for (dst, src) in data.chunks_exact_mut(np).zip(b.chunks_exact(n.max(1))) {
            dst[..n].copy_from_slice(src);
        }
        Self { k, n, np, data }
    }
}

/// `a[m x k] * b` for a packed right operand; returns `m x n`.
pub fn matmul_b(a: &[f32], m: usize, b: &PackedB, scratch: &mut Scratch) -> Vec<f32> {
    let (k, n, np) = (b.k, b.n, b.np);
    let mp = round_up(m.max(1), MR);
    zeroed(&mut scratch.a, mp * k)[..m * k].copy_from_slice(&a[..m * k]);
    if scratch.c.len() < mp * np {
        scratch.c.resize(mp * np, 0.0);
    }
    let c = &mut scratch.c[..mp * np];
    gemm(&scratch.a[..mp * k], &b.data, c, mp, k, np);
    let mut out = Vec::with_capacity(m * n);
    for row in c.chunks_exact(np).take(m) {
        out.extend_from_slice(&row[..n]);
    }
    out
}

/// Reusable buffers (padded operands, results).
#[derive(Default, Debug)]
pub struct Scratch {
    a: Vec<f32>,
    b: Vec<f32>,
    c: Vec<f32>,
}

impl Scratch {
    /// `(b, c)` at once, for callers that fill B then multiply into C.
    pub fn b_and_c(&mut self, nb: usize, nc: usize) -> (&mut [f32], &mut [f32]) {
        zeroed(&mut self.b, nb);
        if self.c.len() < nc {
            self.c.resize(nc, 0.0);
        }
        (&mut self.b[..nb], &mut self.c[..nc])
    }
}

fn zeroed(v: &mut Vec<f32>, n: usize) -> &mut [f32] {
    v.clear();
    v.resize(n, 0.0);
    &mut v[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
        let mut c = vec![0.0f64; m * n];
        for i in 0..m {
            for j in 0..n {
                for t in 0..k {
                    c[i * n + j] += a[i * k + t] as f64 * b[t * n + j] as f64;
                }
            }
        }
        c.into_iter().map(|v| v as f32).collect()
    }

    #[test]
    fn matches_naive_on_odd_sizes() {
        let mut s = Scratch::default();
        for &(m, k, n) in &[(1, 1, 1), (5, 7, 3), (13, 29, 17), (257, 512, 9), (2, 300, 40)] {
            let a: Vec<f32> = (0..m * k).map(|i| ((i * 37 % 101) as f32 - 50.0) / 25.0).collect();
            let b: Vec<f32> = (0..k * n).map(|i| ((i * 53 % 97) as f32 - 48.0) / 30.0).collect();
            let want = naive(&a, &b, m, k, n);
            let got = matmul_b(&a, m, &PackedB::new(&b, k, n), &mut s);
            for (g, w) in got.iter().zip(&want) {
                assert!((g - w).abs() <= 1e-4 * (1.0 + w.abs()), "{m}x{k}x{n}: {g} vs {w}");
            }
            // the padded-A path
            let pa = PackedA::new(&a, m, k);
            let pb = PackedB::new(&b, k, n);
            let mut c = vec![0.0; pa.mp * pb.np];
            gemm(&pa.data, &pb.data, &mut c, pa.mp, k, pb.np);
            for i in 0..m {
                for j in 0..n {
                    let (g, w) = (c[i * pb.np + j], want[i * n + j]);
                    assert!((g - w).abs() <= 1e-4 * (1.0 + w.abs()));
                }
            }
        }
    }
}
