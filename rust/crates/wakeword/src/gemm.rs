//! Small f32 matrix multiply for the convolution / dense layers: a 6x16
//! register-blocked kernel over a row-major A and a pre-packed B, compiled
//! twice (AVX2 + FMA picked at run time on x86-64, plus a portable
//! fallback).

/// Rows of A per micro-tile.
pub const MR: usize = 6;
/// Columns of B per micro-tile.
pub const NR: usize = 16;

pub fn round_up(x: usize, m: usize) -> usize {
    x.div_ceil(m) * m
}

/// `c[mp x np] = a[mp x k] * b` (row-major A and C), `b` packed by
/// [`PackedB`] (`k x np`), `mp % MR == 0`, `np % NR == 0`.
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
    if k == 0 {
        c[..mp * np].fill(0.0);
        return;
    }
    for (it, arows) in a[..mp * k].chunks_exact(MR * k).enumerate() {
        let rows: [&[f32]; MR] = std::array::from_fn(|r| &arows[r * k..(r + 1) * k]);
        for (jt, bpanel) in b[..k * np].chunks_exact(k * NR).enumerate() {
            let mut acc = [[0.0f32; NR]; MR];
            for (kk, bv) in bpanel.chunks_exact(NR).enumerate() {
                for r in 0..MR {
                    let x = rows[r][kk];
                    for t in 0..NR {
                        acc[r][t] = if FMA {
                            x.mul_add(bv[t], acc[r][t])
                        } else {
                            acc[r][t] + x * bv[t]
                        };
                    }
                }
            }
            for (r, row) in acc.iter().enumerate() {
                let off = (it * MR + r) * np + jt * NR;
                c[off..off + NR].copy_from_slice(row);
            }
        }
    }
}

/// A constant right operand (`k x n`), packed into NR-column panels
/// (zero-padded to `np` columns).
#[derive(Clone, Debug)]
pub struct PackedB {
    pub k: usize,
    pub n: usize,
    pub np: usize,
    pub data: Vec<f32>,
}

impl PackedB {
    /// From row-major `b[k x n]`.
    pub fn new(b: &[f32], k: usize, n: usize) -> Self {
        Self::from_fn(k, n, |kk, j| b[kk * n + j])
    }

    /// Element `(kk, j)` is `f(kk, j)`.
    pub fn from_fn(k: usize, n: usize, f: impl Fn(usize, usize) -> f32) -> Self {
        let np = round_up(n.max(1), NR);
        let mut data = vec![0.0; k * np];
        for kk in 0..k {
            for j in 0..n {
                data[((j / NR) * k + kk) * NR + j % NR] = f(kk, j);
            }
        }
        Self { k, n, np, data }
    }
}

/// `a[m x k] * b` for a packed right operand; returns `m x n`.
pub fn matmul_b(a: &[f32], m: usize, b: &PackedB, scratch: &mut Scratch) -> Vec<f32> {
    let (k, n, np) = (b.k, b.n, b.np);
    let mp = round_up(m.max(1), MR);
    let (ap, c) = scratch.bufs(mp * k, mp * np);
    ap[..m * k].copy_from_slice(&a[..m * k]);
    ap[m * k..].fill(0.0);
    gemm(ap, &b.data, c, mp, k, np);
    let mut out = Vec::with_capacity(m * n);
    for row in c.chunks_exact(np).take(m) {
        out.extend_from_slice(&row[..n]);
    }
    out
}

/// Reusable buffers (A operand, result, recycled tensor storage).
#[derive(Default, Debug)]
pub struct Scratch {
    a: Vec<f32>,
    c: Vec<f32>,
    pool: Vec<Vec<f32>>,
}

impl Scratch {
    /// An A buffer of `na` values and a result buffer of `nc` (contents
    /// unspecified).
    pub fn bufs(&mut self, na: usize, nc: usize) -> (&mut [f32], &mut [f32]) {
        if self.a.len() < na {
            self.a.resize(na, 0.0);
        }
        if self.c.len() < nc {
            self.c.resize(nc, 0.0);
        }
        (&mut self.a[..na], &mut self.c[..nc])
    }

    /// An empty vector with room for `n` values, recycled when possible.
    pub fn take(&mut self, n: usize) -> Vec<f32> {
        match self.pool.iter().position(|v| v.capacity() >= n) {
            Some(i) => self.pool.swap_remove(i),
            None => Vec::with_capacity(n),
        }
    }

    /// Return a no longer needed buffer for `take`.
    pub fn give(&mut self, mut v: Vec<f32>) {
        if self.pool.len() < 16 && v.capacity() >= 1024 {
            v.clear();
            self.pool.push(v);
        }
    }
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
        for &(m, k, n) in &[
            (1, 1, 1),
            (5, 7, 3),
            (13, 29, 17),
            (257, 512, 9),
            (2, 300, 40),
            (3, 0, 4),
        ] {
            let a: Vec<f32> = (0..m * k)
                .map(|i| ((i * 37 % 101) as f32 - 50.0) / 25.0)
                .collect();
            let b: Vec<f32> = (0..k * n)
                .map(|i| ((i * 53 % 97) as f32 - 48.0) / 30.0)
                .collect();
            let want = naive(&a, &b, m, k, n);
            let got = matmul_b(&a, m, &PackedB::new(&b, k, n), &mut s);
            assert_eq!(got.len(), m * n);
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    (g - w).abs() <= 1e-4 * (1.0 + w.abs()),
                    "{m}x{k}x{n}: {g} vs {w}"
                );
            }
        }
    }

    #[test]
    #[ignore = "benchmark"]
    fn bench_gemm() {
        for &(m, k, n) in &[(2368, 72, 24), (544, 144, 48), (48, 288, 96), (16, 512, 257)] {
            let a: Vec<f32> = (0..m * k).map(|i| (i % 7) as f32).collect();
            let pb = PackedB::from_fn(k, n, |i, j| ((i + j) % 5) as f32);
            let mp = round_up(m, MR);
            let mut a2 = a.clone();
            a2.resize(mp * k, 0.0);
            let mut c = vec![0.0; mp * pb.np];
            let reps = 200;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                gemm(&a2, &pb.data, &mut c, mp, k, pb.np);
            }
            let s = t.elapsed().as_secs_f64() / reps as f64;
            eprintln!(
                "{m}x{k}x{n}: {:.3} ms, {:.1} GMAC/s",
                s * 1e3,
                (m * k * n) as f64 / s / 1e9
            );
        }
    }
}
