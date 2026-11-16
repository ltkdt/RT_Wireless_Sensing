//! Whole-record resampling onto a uniform time grid.
//!
//! The device clock is not perfectly uniform (packet jitter), so resampling
//! interpolates the **complex I/Q values** over the whole record at once onto a
//! uniform grid $u_j = j / f_s^{target}$, then recomputes amplitude and phase.
//! Amplitude is never interpolated directly, and raw phase is never
//! interpolated (wrap-around discontinuities at $\pm\pi$); interpolating I and
//! Q separately avoids both problems.
//!
//! Interpolating the whole record at once — instead of chunking per second —
//! avoids boundary artifacts (kinks at each chunk edge, spectral leakage into
//! the STFT).
//!
//! Duplicate timestamps are dropped (the first occurrence wins); a tick
//! regression is an error ([`DspError::NonMonotonicTicks`]).
//!
//! [`InterpMethod::CubicSpline`] uses a natural cubic spline
//! ($S''(t_0) = S''(t_{n-1}) = 0$) solved with the Thomas algorithm. No
//! maintained crate evaluates a *non-uniform* natural cubic spline (the
//! `splines` crate's Catmull-Rom requires uniform spacing, which jittered
//! device ticks violate), so it is hand-rolled here and unit-tested against
//! analytic curves.

use ndarray::{Array1, Array2};
use rustfft::num_complex::Complex;

use super::{CsiSignal, DspError};

/// Interpolation method for [`CsiSignal::resample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterpMethod {
    /// Piecewise-constant: take the nearest source sample.
    Nearest,
    /// Piecewise-linear between the two surrounding samples.
    Linear,
    /// Natural cubic spline through all samples (recommended for spectral
    /// work; default).
    #[default]
    CubicSpline,
}

impl CsiSignal {
    /// Resample the whole record onto a uniform grid at `target_fs` Hz using
    /// `method`, interpolating the complex I/Q values per subcarrier.
    ///
    /// The output spans the same duration as the input; `ticks_us` becomes
    /// uniform (derived from the first tick plus $j / f_s^{target}$) and
    /// `fs` is set to `target_fs`. Amplitude and phase are recomputed from the
    /// interpolated I/Q. Duplicate source ticks are dropped; a tick regression
    /// returns [`DspError::NonMonotonicTicks`].
    ///
    /// # Errors
    ///
    /// [`DspError::InvalidConfig`] when `target_fs` is not positive and finite
    /// or the record has fewer than two distinct timestamps.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::{CsiSignal, InterpMethod};
    ///
    /// // 100 Hz sine sampled at 20 Hz (jittered ticks), resampled to 100 Hz.
    /// let fs_src = 20.0;
    /// let n = 41;
    /// let ticks: Array1<u64> = (0..n)
    ///     .map(|i| (i as f64 / fs_src * 1e6) as u64 + if i % 2 == 0 { 137 } else { 0 })
    ///     .collect();
    /// let iq = Array2::from_shape_fn((n, 1), |(i, _)| {
    ///     let t = i as f64 / fs_src;
    ///     Complex::new((2.0 * std::f64::consts::PI * 1.0 * t).sin(), 0.0)
    /// });
    /// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
    ///
    /// let up = signal.resample(100.0, InterpMethod::CubicSpline).unwrap();
    /// assert!((up.fs - 100.0).abs() < 1e-12);
    /// // 41 samples at 20 Hz span 2 s -> 201 samples at 100 Hz.
    /// assert!(up.n_samples() >= 200);
    /// let t0 = up.times_s();
    /// assert!(t0[0].abs() < 1e-12);
    /// ```
    pub fn resample(&self, target_fs: f64, method: InterpMethod) -> Result<CsiSignal, DspError> {
        if !target_fs.is_finite() || target_fs <= 0.0 {
            return Err(DspError::InvalidConfig(format!(
                "target_fs must be positive and finite, got {target_fs}"
            )));
        }
        let n = self.n_samples();
        if n < 2 {
            return Err(DspError::InvalidConfig(
                "need at least 2 samples to resample".to_string(),
            ));
        }

        let t0 = self.ticks_us[0];
        // Source times in seconds, dropping duplicate ticks, erroring on
        // regressions.
        let mut src_t: Vec<f64> = Vec::with_capacity(n);
        let mut src_idx: Vec<usize> = Vec::with_capacity(n);
        for i in 0..n {
            let ti = (self.ticks_us[i] - t0) as f64 / 1e6;
            match src_t.last() {
                Some(&last) if ti < last => {
                    return Err(DspError::NonMonotonicTicks {
                        index: i,
                        previous: self.ticks_us[i - 1],
                        found: self.ticks_us[i],
                    });
                }
                Some(&last) if ti == last => continue, // drop duplicate
                _ => {}
            }
            src_t.push(ti);
            src_idx.push(i);
        }
        if src_t.len() < 2 {
            return Err(DspError::InvalidConfig(
                "fewer than 2 distinct timestamps; cannot interpolate".to_string(),
            ));
        }

        let duration = *src_t.last().unwrap();
        let m = (duration * target_fs).floor() as usize + 1;
        let grid: Vec<f64> = (0..m).map(|j| j as f64 / target_fs).collect();

        let cx = self.complex();
        let sc = self.n_subcarriers();
        let mut out_iq = Array2::<Complex<f64>>::zeros((m, sc));
        for k in 0..sc {
            let re: Vec<f64> = src_idx.iter().map(|&i| cx[[i, k]].re).collect();
            let im: Vec<f64> = src_idx.iter().map(|&i| cx[[i, k]].im).collect();
            let (re_out, im_out) = match method {
                InterpMethod::Nearest => (
                    interp_nearest(&src_t, &re, &grid),
                    interp_nearest(&src_t, &im, &grid),
                ),
                InterpMethod::Linear => (
                    interp_linear(&src_t, &re, &grid),
                    interp_linear(&src_t, &im, &grid),
                ),
                InterpMethod::CubicSpline => {
                    let m_re = natural_cubic_second(&src_t, &re);
                    let m_im = natural_cubic_second(&src_t, &im);
                    (
                        grid.iter().map(|&u| spline_eval(&src_t, &re, &m_re, u)).collect(),
                        grid.iter().map(|&u| spline_eval(&src_t, &im, &m_im, u)).collect(),
                    )
                }
            };
            for j in 0..m {
                out_iq[[j, k]] = Complex::new(re_out[j], im_out[j]);
            }
        }

        // Uniform ticks anchored at the first source tick, kept strictly
        // increasing even if target_fs is huge relative to 1 MHz.
        let mut tick_vec = Vec::with_capacity(m);
        let mut prev = t0;
        for &u in &grid {
            let raw = t0.saturating_add((u * 1e6).round() as u64);
            let v = raw.max(t0).max(if prev > t0 { prev + 1 } else { t0 });
            tick_vec.push(v);
            prev = v;
        }
        let ticks = Array1::from_vec(tick_vec);

        let mut signal = CsiSignal::from_iq(out_iq, ticks)?;
        signal.fs = target_fs;
        Ok(signal)
    }
}

/// Index of the last source point `<= u`, clamped to `0..=n-2`.
fn segment_index(t: &[f64], u: f64) -> usize {
    let n = t.len();
    let pp = t.partition_point(|&x| x <= u);
    pp.saturating_sub(1).min(n - 2)
}

/// Nearest-neighbor interpolation of `y` at the query points `grid`.
fn interp_nearest(t: &[f64], y: &[f64], grid: &[f64]) -> Vec<f64> {
    grid.iter()
        .map(|&u| {
            let i = segment_index(t, u);
            if u - t[i] <= t[i + 1] - u {
                y[i]
            } else {
                y[i + 1]
            }
        })
        .collect()
}

/// Piecewise-linear interpolation of `y` at the query points `grid`.
/// Values outside the source range are clamped to the endpoints.
fn interp_linear(t: &[f64], y: &[f64], grid: &[f64]) -> Vec<f64> {
    grid.iter()
        .map(|&u| {
            if u <= t[0] {
                return y[0];
            }
            let n = t.len();
            if u >= t[n - 1] {
                return y[n - 1];
            }
            let i = segment_index(t, u);
            let w = (u - t[i]) / (t[i + 1] - t[i]);
            y[i] * (1.0 - w) + y[i + 1] * w
        })
        .collect()
}

/// Second derivatives $M_i$ of the natural cubic spline through `(t, y)`,
/// solved with the Thomas algorithm. Natural boundary: $M_0 = M_{n-1} = 0$.
///
/// For interior points the tridiagonal system is
///
/// $$h_{i-1} M_{i-1} + 2(h_{i-1} + h_i) M_i + h_i M_{i+1}
///    = 6\left(\frac{y_{i+1} - y_i}{h_i} - \frac{y_i - y_{i-1}}{h_{i-1}}\right)$$
///
/// with $h_i = t_{i+1} - t_i$. With two points the spline degenerates to the
/// straight line (all $M_i = 0$).
fn natural_cubic_second(t: &[f64], y: &[f64]) -> Vec<f64> {
    let n = t.len();
    let mut m = vec![0.0; n];
    if n <= 2 {
        return m;
    }
    let h: Vec<f64> = (0..n - 1).map(|i| t[i + 1] - t[i]).collect();
    let interior = n - 2;
    let mut cp = vec![0.0; interior];
    let mut dp = vec![0.0; interior];
    for idx in 0..interior {
        let i = idx + 1;
        let a = if idx == 0 { 0.0 } else { h[i - 1] };
        let b = 2.0 * (h[i - 1] + h[i]);
        let c = if idx == interior - 1 { 0.0 } else { h[i] };
        let d = 6.0 * ((y[i + 1] - y[i]) / h[i] - (y[i] - y[i - 1]) / h[i - 1]);
        if idx == 0 {
            cp[0] = c / b;
            dp[0] = d / b;
        } else {
            let denom = b - a * cp[idx - 1];
            cp[idx] = c / denom;
            dp[idx] = (d - a * dp[idx - 1]) / denom;
        }
    }
    // Back substitution; m[n-1] stays 0 (natural boundary).
    for idx in (0..interior).rev() {
        m[idx + 1] = dp[idx] - cp[idx] * m[idx + 2];
    }
    m
}

/// Evaluate the natural cubic spline at `u`:
///
/// $$S(u) = a\,y_i + b\,y_{i+1}
///    + \frac{h_i^2}{6}\left[ (a^3 - a) M_i + (b^3 - b) M_{i+1} \right]$$
///
/// with $a = (t_{i+1} - u)/h_i$, $b = (u - t_i)/h_i$. Clamped to the endpoint
/// values outside the source range.
fn spline_eval(t: &[f64], y: &[f64], m: &[f64], u: f64) -> f64 {
    let n = t.len();
    if u <= t[0] {
        return y[0];
    }
    if u >= t[n - 1] {
        return y[n - 1];
    }
    let i = segment_index(t, u);
    let h = t[i + 1] - t[i];
    let a = (t[i + 1] - u) / h;
    let b = (u - t[i]) / h;
    a * y[i] + b * y[i + 1] + ((a * a * a - a) * m[i] + (b * b * b - b) * m[i + 1]) * h * h / 6.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jittered_ticks(n: usize, fs: f64) -> Array1<u64> {
        (0..n)
            .map(|i| {
                let base = i as f64 / fs * 1e6;
                let jitter = if i % 3 == 0 { 211.0 } else { -97.0 };
                (base + jitter).round().max(0.0) as u64
            })
            .collect()
    }

    fn sine_signal(n: usize, sc: usize, fs: f64, freq: f64, amp: f64) -> CsiSignal {
        let ticks = jittered_ticks(n, fs);
        // Sample the sine at the *actual* (jittered) tick times, relative to
        // the first tick, so the signal phase matches `times_s()` exactly.
        let iq = Array2::from_shape_fn((n, sc), |(i, k)| {
            let t = (ticks[i] - ticks[0]) as f64 / 1e6;
            let f = freq + 0.05 * k as f64;
            let v = amp * (2.0 * std::f64::consts::PI * f * t).sin();
            Complex::new(v, 0.0)
        });
        CsiSignal::from_iq(iq, ticks).unwrap()
    }

    #[test]
    fn spline_passes_through_all_knots() {
        let t: Vec<f64> = (0..20).map(|i| i as f64 * 0.37 + (i % 4) as f64 * 0.05).collect();
        let y: Vec<f64> = t.iter().map(|&x| (x * 2.0).sin()).collect();
        let m = natural_cubic_second(&t, &y);
        for i in 0..t.len() {
            assert!((spline_eval(&t, &y, &m, t[i]) - y[i]).abs() < 1e-9);
        }
    }

    #[test]
    fn spline_reproduces_a_linear_function_exactly() {
        let t: Vec<f64> = vec![0.0, 0.3, 0.9, 1.7, 2.0, 3.3];
        let y: Vec<f64> = t.iter().map(|&x| 2.0 * x - 1.0).collect();
        let m = natural_cubic_second(&t, &y);
        for u in [0.1, 0.5, 1.0, 1.55, 2.9, 3.29] {
            assert!((spline_eval(&t, &y, &m, u) - (2.0 * u - 1.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn linear_interp_matches_endpoints_and_midpoint() {
        let t = vec![0.0, 1.0, 3.0];
        let y = vec![0.0, 10.0, 20.0];
        let grid = vec![0.0, 0.5, 2.0, 3.0, 4.0];
        let out = interp_linear(&t, &y, &grid);
        assert!((out[0] - 0.0).abs() < 1e-12);
        assert!((out[1] - 5.0).abs() < 1e-12);
        assert!((out[2] - 15.0).abs() < 1e-12);
        assert!((out[3] - 20.0).abs() < 1e-12);
        assert!((out[4] - 20.0).abs() < 1e-12); // clamped
    }

    #[test]
    fn resample_updates_fs_and_grid() {
        let signal = sine_signal(41, 2, 20.0, 1.0, 1.0);
        let up = signal.resample(100.0, InterpMethod::CubicSpline).unwrap();
        assert!((up.fs - 100.0).abs() < 1e-12);
        let times = up.times_s();
        for w in times.windows(2).into_iter() {
            assert!(w[1] >= w[0]);
        }
        // duration preserved
        let dur_in = signal.duration_s();
        let dur_out = up.duration_s();
        assert!((dur_out - dur_in).abs() < 1.0 / 100.0 + 1e-9);
        // first grid point coincides with the first source sample
        assert!((up.amplitude[[0, 0]] - signal.amplitude[[0, 0]]).abs() < 1e-12);
    }

    #[test]
    fn spline_is_more_accurate_than_linear_on_smooth_curve() {
        let fs_src = 20.0;
        let n = 41;
        let signal = sine_signal(n, 1, fs_src, 2.0, 1.0);
        let target = 100.0;
        let spline = signal.resample(target, InterpMethod::CubicSpline).unwrap();
        let linear = signal.resample(target, InterpMethod::Linear).unwrap();
        // Compare the interpolated complex I component against the analytic
        // sine (amplitude is a magnitude, so it cannot be compared directly).
        let cx_spline = spline.complex();
        let cx_linear = linear.complex();
        let mut err_spline = 0.0f64;
        let mut err_linear = 0.0f64;
        for j in 0..spline.n_samples() {
            let t = spline.times_s()[j];
            let truth = (2.0 * std::f64::consts::PI * 2.0 * t).sin();
            err_spline += (cx_spline[[j, 0]].re - truth).abs();
            err_linear += (cx_linear[[j, 0]].re - truth).abs();
        }
        assert!(err_spline < err_linear);
        assert!(err_spline / (spline.n_samples() as f64) < 1e-3);
    }

    #[test]
    fn resample_recovers_ramp_exactly_with_linear() {
        // A linear ramp is exact under linear interpolation on any grid.
        let n = 11;
        let ticks = Array1::from_iter((0..n).map(|i| i as u64 * 50_000));
        let iq = Array2::from_shape_fn((n, 1), |(i, _)| Complex::new(0.5 * i as f64, 0.0));
        let signal = CsiSignal::from_iq(iq, ticks).unwrap();
        // Same rate: 0.5 s at 20 Hz -> 11 samples on the same grid.
        let out = signal.resample(20.0, InterpMethod::Linear).unwrap();
        assert_eq!(out.n_samples(), n);
        for j in 0..out.n_samples() {
            assert!((out.amplitude[[j, 0]] - 0.5 * j as f64).abs() < 1e-9);
        }
    }

    #[test]
    fn duplicate_ticks_are_dropped() {
        let mut ticks = Array1::from_iter((0..10).map(|i| i as u64 * 50_000));
        ticks[5] = ticks[4]; // duplicate
        let iq = Array2::from_shape_fn((10, 1), |(i, _)| Complex::new(i as f64, 0.0));
        let signal = CsiSignal::from_iq(iq, ticks).unwrap();
        let out = signal.resample(20.0, InterpMethod::Linear).unwrap();
        assert!(out.n_samples() >= 9);
        let times = out.times_s();
        for w in times.windows(2).into_iter() {
            assert!(w[1] > w[0]);
        }
    }

    #[test]
    fn regressing_ticks_are_an_error() {
        // Build a valid signal first, then corrupt its ticks directly.
        let signal = sine_signal(10, 1, 20.0, 1.0, 1.0);
        let mut bad = signal.clone();
        bad.ticks_us[7] = bad.ticks_us[6] - 1;
        let result = bad.resample(50.0, InterpMethod::Linear);
        assert!(matches!(result, Err(DspError::NonMonotonicTicks { index: 7, .. })));
    }

    #[test]
    fn invalid_target_fs_is_rejected() {
        let signal = sine_signal(10, 1, 20.0, 1.0, 1.0);
        for bad_fs in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                signal.resample(bad_fs, InterpMethod::Linear),
                Err(DspError::InvalidConfig(_))
            ));
        }
    }

    #[test]
    fn nearest_picks_closest_sample() {
        let t = vec![0.0, 1.0, 2.0];
        let y = vec![10.0, 20.0, 30.0];
        let grid = vec![0.4, 0.6, 1.4, 1.6];
        let out = interp_nearest(&t, &y, &grid);
        assert_eq!(out, vec![10.0, 20.0, 20.0, 30.0]);
    }
}
