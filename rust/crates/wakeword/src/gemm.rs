//! Small f32 matrix multiply for the convolution / dense layers: a 6x16
//! register-blocked kernel over a row-major A and a pre-packed B, compiled
//! twice (AVX2 + FMA picked at run time on x86-64, plus a portable
//! fallback).

/// Rows of A per micro-tile.
pub const MR: usize = 6;
/// Columns of B per (full) panel; the last panel may be NR / 2 wide.
pub const NR: usize = 16;
const NH: usize = NR / 2;

pub fn round_up(x: usize, m: usize) -> usize {
    x.div_ceil(m) * m
}

/// `c[m x np] = a[m x k] * b` (row-major A and C, `b` packed by
/// [`PackedB`]); columns `b.n..b.np` of C come out as zero.
pub fn gemm(a: &[f32], m: usize, b: &PackedB, c: &mut [f32]) {
    assert!(a.len() >= m * b.k && c.len() >= m * b.np);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU supports AVX2 and FMA (checked just above).
        unsafe { gemm_avx2(a, m, b, c) };
        return;
    }
    driver::<{ cfg!(target_feature = "fma") }>(a, m, b, c);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
fn gemm_avx2(a: &[f32], m: usize, b: &PackedB, c: &mut [f32]) {
    driver::<true>(a, m, b, c);
}

#[inline(always)]
fn driver<const FMA: bool>(a: &[f32], m: usize, b: &PackedB, c: &mut [f32]) {
    let (k, np) = (b.k, b.np);
    if k == 0 {
        c[..m * np].fill(0.0);
        return;
    }
    let full = np / NR;
    // full-width panels: 6 x 16 tiles (12 accumulators), smaller at the end
    let mut i = 0;
    while i < m {
        let left = m - i;
        i += if left >= MR {
            block::<FMA, MR, NR>(a, i, b, 0..full, c)
        } else if left >= 4 {
            block::<FMA, 4, NR>(a, i, b, 0..full, c)
        } else if left >= 2 {
            block::<FMA, 2, NR>(a, i, b, 0..full, c)
        } else {
            block::<FMA, 1, NR>(a, i, b, 0..full, c)
        };
    }
    // half-width last panel: 12 x 8 tiles keep as many accumulators busy
    if np % NR != 0 {
        let mut i = 0;
        while i < m {
            let left = m - i;
            i += if left >= 2 * MR {
                block::<FMA, { 2 * MR }, NH>(a, i, b, full..full + 1, c)
            } else if left >= MR {
                block::<FMA, MR, NH>(a, i, b, full..full + 1, c)
            } else if left >= 2 {
                block::<FMA, 2, NH>(a, i, b, full..full + 1, c)
            } else {
                block::<FMA, 1, NH>(a, i, b, full..full + 1, c)
            };
        }
    }
}

/// Rows `i..i + M` of C for panels `panels` (all of width N); returns M.
#[inline(always)]
fn block<const FMA: bool, const M: usize, const N: usize>(
    a: &[f32],
    i: usize,
    b: &PackedB,
    panels: std::ops::Range<usize>,
    c: &mut [f32],
) -> usize {
    let (k, np) = (b.k, b.np);
    let ar: [&[f32]; M] = std::array::from_fn(|r| &a[(i + r) * k..(i + r + 1) * k]);
    for jt in panels {
        let p0 = jt * k * NR;
        let acc = tile::<FMA, M, N>(&ar, &b.data[p0..p0 + k * N]);
        for (r, row) in acc.iter().enumerate() {
            let off = (i + r) * np + jt * NR;
            c[off..off + N].copy_from_slice(row);
        }
    }
    M
}

#[inline(always)]
fn tile<const FMA: bool, const M: usize, const N: usize>(
    ar: &[&[f32]; M],
    panel: &[f32],
) -> [[f32; N]; M] {
    let mut acc = [[0.0f32; N]; M];
    for (kk, bv) in panel.as_chunks::<N>().0.iter().enumerate() {
        for r in 0..M {
            let x = ar[r][kk];
            for t in 0..N {
                acc[r][t] = if FMA {
                    x.mul_add(bv[t], acc[r][t])
                } else {
                    acc[r][t] + x * bv[t]
                };
            }
        }
    }
    acc
}

/// A constant right operand (`k x n`), packed into NR-column panels (the
/// last one NR / 2 wide when that suffices), zero-padded to `np` columns.
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
        let np = round_up(n.max(1), NH);
        let full = np / NR;
        let mut data = vec![0.0; k * np];
        for kk in 0..k {
            for j in 0..n {
                let (jt, t) = (j / NR, j % NR);
                let w = if jt < full { NR } else { NH };
                data[jt * k * NR + kk * w + t] = f(kk, j);
            }
        }
        Self { k, n, np, data }
    }
}

/// `a[m x k] * b` for a packed right operand; returns `m x n`.
pub fn matmul_b(a: &[f32], m: usize, b: &PackedB, scratch: &mut Scratch) -> Vec<f32> {
    let (n, np) = (b.n, b.np);
    let c = scratch.c(m * np);
    gemm(a, m, b, c);
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
    /// A result buffer of `nc` values (contents unspecified).
    pub fn c(&mut self, nc: usize) -> &mut [f32] {
        if self.c.len() < nc {
            self.c.resize(nc, 0.0);
        }
        &mut self.c[..nc]
    }

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
            (7, 5, 24),
            (12, 33, 72),
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
        for &(m, k, n) in &[
            (2368, 72, 24),
            (544, 144, 48),
            (48, 288, 96),
            (8, 512, 257),
            (1, 1536, 128),
        ] {
            let a: Vec<f32> = (0..m * k).map(|i| (i % 7) as f32).collect();
            let pb = PackedB::from_fn(k, n, |i, j| ((i + j) % 5) as f32);
            let mut c = vec![0.0; m * pb.np];
            let reps = 200;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                gemm(&a, m, &pb, &mut c);
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
