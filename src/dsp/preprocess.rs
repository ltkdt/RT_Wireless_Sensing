//! Preprocessing: per-subcarrier normalization and the CSI-ratio helper.
//!
//! Normalization removes static per-subcarrier offsets (different subcarriers
//! see different static attenuation) so that downstream analysis works on
//! comparable scales. With [`NormScheme::PerSubcarrier`] each subcarrier
//! column $x_k$ is standardized as
//!
//! $$z_{t,k} = \frac{x_{t,k} - \mu_k}{\sigma_k + \varepsilon}$$
//!
//! where the epsilon guard $\varepsilon = 10^{-12}$ keeps constant (near-zero
//! variance) subcarriers finite instead of dividing by zero.
//!
//! [`csi_ratio`] computes the conjugate product $r_k(t) = h_k(t)\, h_k^*(t-1)$,
//! a standard phase-insensitive preprocessing on ESP32 platforms: it removes
//! the static phase offset while keeping phase *differences* caused by motion.

use ndarray::Array2;

use super::{CsiSignal, EPS};

/// Normalization scheme used by [`CsiSignal::normalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NormScheme {
    /// Standardize each subcarrier column to zero mean and unit variance:
    /// $z_{t,k} = (x_{t,k} - \mu_k) / (\sigma_k + \varepsilon)$.
    #[default]
    PerSubcarrier,
    /// Standardize the whole matrix at once (CSI-Bench style):
    /// $z = (x - \mu) / (\sigma + \varepsilon)$ over all samples and subcarriers.
    PerSample,
    /// Scale each subcarrier column to $[0, 1]$:
    /// $z_{t,k} = (x_{t,k} - \min_k) / (\max_k - \min_k + \varepsilon)$.
    MinMax,
}

impl CsiSignal {
    /// Normalize the amplitude matrix according to `scheme`.
    ///
    /// Only `amplitude` is transformed; phase, ticks, `fs`, and the complex
    /// view are kept consistent. The epsilon guard means a constant subcarrier
    /// (zero variance) maps to zeros instead of panicking or producing NaN.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rt_wireless_sensing::dsp::{CsiSignal, NormScheme};
    ///
    /// // Subcarrier 0 is constant (zero variance), subcarrier 1 varies.
    /// let amplitude = Array2::from_shape_fn((5, 2), |(i, k)| {
    ///     if k == 0 { 7.0 } else { i as f64 }
    /// });
    /// let phase = Array2::zeros((5, 2));
    /// let ticks = Array1::from_iter((0..5).map(|i| i as u64 * 50_000));
    /// let signal = CsiSignal::new(amplitude, phase, ticks).unwrap();
    ///
    /// let z = signal.normalize(NormScheme::PerSubcarrier);
    /// // Constant column is finite and mapped to zero, not NaN.
    /// assert!(z.amplitude.column(0).iter().all(|v| v.is_finite() && *v == 0.0));
    /// // Varying column has zero mean and unit std.
    /// let col = z.amplitude.column(1);
    /// let mean = col.sum() / col.len() as f64;
    /// let var = col.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / col.len() as f64;
    /// assert!(mean.abs() < 1e-9);
    /// assert!((var - 1.0).abs() < 1e-9);
    /// ```
    pub fn normalize(&self, scheme: NormScheme) -> Self {
        let (n, sc) = self.amplitude.dim();
        // Null subcarriers carry no channel information: they are excluded from
        // the statistics behind every scheme and written back as 0. With an
        // empty `null_bins` the code below is a plain whole-matrix pass, so the
        // output is bit-identical to the null-unaware behaviour.
        let null = &self.null_bins;
        let is_null = |k: usize| null.contains(&k);
        let out = match scheme {
            NormScheme::PerSubcarrier => {
                let mut out = Array2::<f64>::zeros((n, sc));
                for k in 0..sc {
                    if is_null(k) {
                        continue;
                    }
                    let col = self.amplitude.column(k);
                    let mean = col.sum() / n as f64;
                    let var =
                        col.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
                    let denom = var.sqrt() + EPS;
                    out.column_mut(k)
                        .assign(&col.mapv(|v| (v - mean) / denom));
                }
                out
            }
            NormScheme::PerSample => {
                let (sum, count) = self.accumulate_non_null(null, |v| v);
                let mean = if count == 0 { 0.0 } else { sum / count as f64 };
                let (squares, _) = self.accumulate_non_null(null, |v| (v - mean).powi(2));
                let denom = if count == 0 {
                    EPS
                } else {
                    (squares / count as f64).sqrt() + EPS
                };
                let mut out = Array2::<f64>::zeros((n, sc));
                for k in 0..sc {
                    if is_null(k) {
                        continue;
                    }
                    out.column_mut(k).assign(
                        &self.amplitude.column(k).mapv(|v| (v - mean) / denom),
                    );
                }
                out
            }
            NormScheme::MinMax => {
                let mut out = Array2::<f64>::zeros((n, sc));
                for k in 0..sc {
                    if is_null(k) {
                        continue;
                    }
                    let col = self.amplitude.column(k);
                    let min = col.iter().copied().fold(f64::INFINITY, f64::min);
                    let max = col.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let denom = max - min + EPS;
                    out.column_mut(k).assign(&col.mapv(|v| (v - min) / denom));
                }
                out
            }
        };
        self.with_amplitude(out)
    }

    /// Sum `f(value)` and count the samples over non-null subcarriers, in row
    /// order. Order matches [`ndarray::Array2::iter`], so the accumulation is
    /// the same one a whole-matrix pass would perform.
    fn accumulate_non_null(
        &self,
        null: &[usize],
        f: impl Fn(f64) -> f64,
    ) -> (f64, usize) {
        let (n, sc) = self.amplitude.dim();
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for i in 0..n {
            for k in 0..sc {
                if null.contains(&k) {
                    continue;
                }
                sum += f(self.amplitude[[i, k]]);
                count += 1;
            }
        }
        (sum, count)
    }
}

/// Compute the CSI ratio $r_k(t) = h_k(t)\, h_k^*(t-1)$ per subcarrier.
///
/// This is the standard phase-insensitive preprocessing for ESP32 CSI: the
/// static phase offset cancels in the conjugate product, leaving
/// $r_k(t) = |h_k(t)|\,|h_k(t-1)|\, e^{j(\varphi_k(t) - \varphi_k(t-1))}$.
/// The first sample has no predecessor, so it is set to
/// $r_k(0) = |h_k(0)|^2$ (real, positive), keeping the output the same length
/// as the input. The result's amplitude and phase are recomputed from the
/// ratio; ticks and `fs` are unchanged.
///
/// This is an optional helper: if you do not need it, phase output stays raw.
///
/// # Example
///
/// ```
/// use ndarray::{Array1, Array2};
/// use rustfft::num_complex::Complex;
/// use rt_wireless_sensing::dsp::{CsiSignal, csi_ratio};
///
/// // Constant-amplitude signal rotating by w per sample: h_t = A e^{j w t}.
/// let a = 2.0f64;
/// let w = 0.3f64;
/// let n = 8;
/// let iq = Array2::from_shape_fn((n, 1), |(t, _)| {
///     Complex::new(a * (w * t as f64).cos(), a * (w * t as f64).sin())
/// });
/// let ticks = Array1::from_iter((0..n).map(|i| i as u64 * 50_000));
/// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
///
/// let ratio = csi_ratio(&signal);
/// // |r| = A^2 and the phase is the constant rotation w.
/// assert!((ratio.amplitude[[3, 0]] - a * a).abs() < 1e-9);
/// assert!((ratio.phase[[3, 0]] - w).abs() < 1e-9);
/// ```
pub fn csi_ratio(signal: &CsiSignal) -> CsiSignal {
    let cx = signal.complex();
    let (n, sc) = cx.dim();
    let mut out = Array2::<rustfft::num_complex::Complex<f64>>::zeros((n, sc));
    for k in 0..sc {
        out[[0, k]] = cx[[0, k]] * cx[[0, k]].conj();
        for t in 1..n {
            out[[t, k]] = cx[[t, k]] * cx[[t - 1, k]].conj();
        }
    }
    let amplitude = out.mapv(|c| c.norm());
    let phase = out.mapv(|c| c.arg());
    let mut result = CsiSignal {
        amplitude,
        phase,
        ticks_us: signal.ticks_us.clone(),
        fs: signal.fs,
        iq: Some(out),
        null_bins: signal.null_bins.clone(),
    };
    // n >= 1 always holds for a constructed CsiSignal; keep ticks valid.
    result.ticks_us = signal.ticks_us.clone();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::Array1;
    use rustfft::num_complex::Complex;

    fn signal_with_columns(cols: Vec<Vec<f64>>) -> CsiSignal {
        let n = cols[0].len();
        let sc = cols.len();
        let amplitude =
            Array2::from_shape_fn((n, sc), |(i, k)| cols[k][i]);
        let phase = Array2::zeros((n, sc));
        let ticks = Array1::from_iter((0..n).map(|i| i as u64 * 50_000));
        CsiSignal::new(amplitude, phase, ticks).unwrap()
    }

    #[test]
    fn per_subcarrier_gives_zero_mean_unit_std() {
        let signal = signal_with_columns(vec![vec![1.0, 2.0, 3.0, 4.0, 10.0]]);
        let z = signal.normalize(NormScheme::PerSubcarrier);
        let col = z.amplitude.column(0);
        let mean = col.sum() / col.len() as f64;
        let std = (col.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / col.len() as f64).sqrt();
        assert!(mean.abs() < 1e-12);
        assert!((std - 1.0).abs() < 1e-9);
    }

    #[test]
    fn constant_subcarrier_does_not_panic_or_nan() {
        let signal = signal_with_columns(vec![vec![5.0; 6], vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]]);
        let z = signal.normalize(NormScheme::PerSubcarrier);
        assert!(z.amplitude.iter().all(|v| v.is_finite()));
        assert!(z.amplitude.column(0).iter().all(|&v| v == 0.0));
        let mm = signal.normalize(NormScheme::MinMax);
        assert!(mm.amplitude.column(0).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn min_max_scales_to_unit_interval() {
        let signal = signal_with_columns(vec![vec![2.0, 4.0, 6.0, 8.0]]);
        let z = signal.normalize(NormScheme::MinMax);
        let col = z.amplitude.column(0);
        assert!((col[0] - 0.0).abs() < 1e-9);
        assert!((col[3] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn per_sample_uses_global_statistics() {
        let signal = signal_with_columns(vec![vec![0.0, 0.0, 0.0], vec![3.0, 3.0, 3.0]]);
        let z = signal.normalize(NormScheme::PerSample);
        // global mean = 1.5, std = 1.5 -> values become -1 and +1
        assert!((z.amplitude[[0, 0]] + 1.0).abs() < 1e-9);
        assert!((z.amplitude[[0, 1]] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_preserves_ticks_fs_and_phase() {
        let signal = signal_with_columns(vec![vec![1.0, 2.0, 3.0, 4.0]]);
        let z = signal.normalize(NormScheme::PerSubcarrier);
        assert_eq!(z.ticks_us, signal.ticks_us);
        assert_eq!(z.fs, signal.fs);
        assert_eq!(z.phase, signal.phase);
    }

    /// A column of large values plus a null bin full of garbage: the null bin
    /// must not pull any statistic.
    fn signal_with_null_bin() -> CsiSignal {
        let mut signal = signal_with_columns(vec![
            vec![1.0, 2.0, 3.0, 4.0],       // subcarrier 0 (kept)
            vec![1000.0, -500.0, 0.0, 250.0], // subcarrier 1 (null)
            vec![5.0, 6.0, 7.0, 8.0],       // subcarrier 2 (kept)
        ]);
        signal.null_bins = vec![1];
        signal
    }

    #[test]
    fn null_bins_are_written_as_zero_for_every_scheme() {
        let signal = signal_with_null_bin();
        for scheme in [
            NormScheme::PerSubcarrier,
            NormScheme::PerSample,
            NormScheme::MinMax,
        ] {
            let z = signal.normalize(scheme);
            assert!(
                z.amplitude.column(1).iter().all(|&v| v == 0.0),
                "{scheme:?} must write the null column as zeros"
            );
            assert!(z.amplitude.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn per_sample_ignores_null_columns_in_mean_and_std() {
        let signal = signal_with_null_bin();
        let z = signal.normalize(NormScheme::PerSample);
        // Hand-computed over subcarriers 0 and 2 only: values 1..4 and 5..8.
        let kept = [1.0, 5.0, 2.0, 6.0, 3.0, 7.0, 4.0, 8.0];
        let mean = kept.iter().sum::<f64>() / kept.len() as f64;
        let std = (kept.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / kept.len() as f64)
            .sqrt();
        for (i, k) in [(0, 0), (1, 0), (2, 2), (3, 2)] {
            let expected = (signal.amplitude[[i, k]] - mean) / (std + super::EPS);
            assert!((z.amplitude[[i, k]] - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn per_subcarrier_ignores_other_null_columns() {
        let with_null = signal_with_null_bin();
        let mut without = with_null.clone();
        // Corrupt the null column further: a kept column's statistics must not
        // move, because null columns are excluded outright.
        for i in 0..4 {
            without.amplitude[[i, 1]] *= 37.0;
        }
        let a = with_null.normalize(NormScheme::PerSubcarrier);
        let b = without.normalize(NormScheme::PerSubcarrier);
        for i in 0..4 {
            for k in [0, 2] {
                assert!((a.amplitude[[i, k]] - b.amplitude[[i, k]]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn normalize_is_unchanged_when_null_bins_is_empty() {
        let signal = signal_with_columns(vec![
            vec![1.0, 2.0, 3.0, 4.0, 10.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0],
            vec![-3.0, 7.0, 2.0, 2.0, 1.0],
        ]);
        assert!(signal.null_bins.is_empty());
        for scheme in [
            NormScheme::PerSubcarrier,
            NormScheme::PerSample,
            NormScheme::MinMax,
        ] {
            // Reference implementation of the null-unaware formulas.
            let (n, sc) = signal.amplitude.dim();
            let mut expected = ndarray::Array2::<f64>::zeros((n, sc));
            match scheme {
                NormScheme::PerSubcarrier | NormScheme::MinMax => {
                    for k in 0..sc {
                        let col = signal.amplitude.column(k);
                        if scheme == NormScheme::PerSubcarrier {
                            let mean = col.sum() / n as f64;
                            let var = col.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                                / n as f64;
                            let denom = var.sqrt() + super::EPS;
                            expected.column_mut(k).assign(&col.mapv(|v| (v - mean) / denom));
                        } else {
                            let min = col.iter().copied().fold(f64::INFINITY, f64::min);
                            let max = col.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                            let denom = max - min + super::EPS;
                            expected
                                .column_mut(k)
                                .assign(&col.mapv(|v| (v - min) / denom));
                        }
                    }
                }
                NormScheme::PerSample => {
                    let total = (n * sc) as f64;
                    let mean = signal.amplitude.sum() / total;
                    let var = signal
                        .amplitude
                        .iter()
                        .map(|v| (v - mean).powi(2))
                        .sum::<f64>()
                        / total;
                    let denom = var.sqrt() + super::EPS;
                    expected = signal.amplitude.mapv(|v| (v - mean) / denom);
                }
            }
            let got = signal.normalize(scheme);
            for i in 0..n {
                for k in 0..sc {
                    assert!(
                        (got.amplitude[[i, k]] - expected[[i, k]]).abs() < 1e-12,
                        "{scheme:?} differs at ({i}, {k})"
                    );
                }
            }
        }
    }

    #[test]
    fn normalize_keeps_null_bins_on_the_result() {
        let signal = signal_with_null_bin();
        let z = signal.normalize(NormScheme::PerSubcarrier);
        assert_eq!(z.null_bins, vec![1]);
        assert_eq!(csi_ratio(&signal).null_bins, vec![1]);
    }

    #[test]
    fn csi_ratio_of_rotating_phasor_is_constant() {
        let a = 3.0f64;
        let w = -0.2f64;
        let n = 16;
        let iq = Array2::from_shape_fn((n, 2), |(t, k)| {
            let ww = w * (1.0 + 0.1 * k as f64);
            Complex::new(a * (ww * t as f64).cos(), a * (ww * t as f64).sin())
        });
        let ticks = Array1::from_iter((0..n).map(|i| i as u64 * 50_000));
        let signal = CsiSignal::from_iq(iq, ticks).unwrap();
        let r = csi_ratio(&signal);
        assert_eq!(r.n_samples(), n);
        for k in 0..2 {
            let ww = w * (1.0 + 0.1 * k as f64);
            for t in 1..n {
                assert!((r.amplitude[[t, k]] - a * a).abs() < 1e-9);
                assert!((r.phase[[t, k]] - ww).abs() < 1e-9);
            }
        }
        // First sample: |h0|^2, phase 0.
        assert!((r.amplitude[[0, 0]] - a * a).abs() < 1e-9);
        assert!(r.phase[[0, 0]].abs() < 1e-12);
    }
}
