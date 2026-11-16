//! Outlier removal and frequency-domain biquad filters.
//!
//! # Outlier removal (time domain)
//!
//! All three operate on a 1-D slice, centered at each sample with the window
//! shrunk at the edges (a sample near the boundary uses the `k` nearest
//! samples that exist). `k` is rounded up to the next odd number so the window
//! has a well-defined center.
//!
//! * [`moving_average`]: $y_i = \frac{1}{m}\sum_{j \in W_i} x_j$
//! * [`exp_moving_average`]: $y_i = \alpha x_i + (1-\alpha) y_{i-1}$ with
//!   $\alpha = 2/(k+1)$ (EWMA)
//! * [`median_filter`]: $y_i = \operatorname{median}(W_i)$
//! * [`hampel_filter`]: the standard CSI outlier remover — replace $x_i$ with
//!   the window median when $|x_i - m_i| > n_\sigma\, \sigma_i$, where
//!   $m_i$ is the window median and
//!   $\sigma_i = 1.4826 \cdot \operatorname{median}_{j \in W_i} |x_j - m_i|$
//!   is the robust (MAD-based) standard deviation.
//!
//! # Frequency-domain filters (biquads)
//!
//! [`lowpass`], [`highpass`], and [`bandpass`] apply second-order RBJ biquads
//! via the `biquad` crate (`DirectForm2Transposed`, Butterworth
//! $Q = 1/\sqrt{2}$):
//!
//! $$y[n] = b_0 x[n] + b_1 x[n-1] + b_2 x[n-2] - a_1 y[n-1] - a_2 y[n-2]$$
//!
//! [`bandpass`] is a cascade of a high-pass at `lo_hz` and a low-pass at
//! `hi_hz`. Note the startup transient: the first few samples of a filtered
//! signal are not yet steady-state. Typical use: BPF 0.1–0.5 Hz to isolate
//! breathing; HPF above ~0.2 Hz to remove static-object reflections before
//! motion/gesture analysis.
//!
//! The [`CsiSignal`] methods apply the corresponding slice function to every
//! amplitude subcarrier column.

use biquad::{Biquad, Coefficients, DirectForm2Transposed, Hertz, Q_BUTTERWORTH_F64, Type};

use super::{CsiSignal, DspError};

/// Round `k` up to the next odd window size (>= 1).
fn odd_window(k: usize) -> Result<usize, DspError> {
    if k == 0 {
        return Err(DspError::InvalidConfig("window size k must be >= 1".to_string()));
    }
    Ok(if k % 2 == 1 { k } else { k + 1 })
}

fn check_signal(x: &[f64]) -> Result<(), DspError> {
    if x.is_empty() {
        return Err(DspError::EmptySignal);
    }
    Ok(())
}

/// Centered window bounds `[lo, hi)` around `i` for a window of `k` samples,
/// shrunk at the edges.
fn window_bounds(n: usize, i: usize, k: usize) -> (usize, usize) {
    let half = k / 2;
    let lo = i.saturating_sub(half);
    let hi = (i + half + 1).min(n);
    (lo, hi)
}

/// Median of a non-empty slice.
///
/// Hand-rolled with `select_nth_unstable` (quickselect, average O(k)): the
/// `ndarray-stats` 0.7 quantile API is in-place on `ArrayRef` and does not fit
/// read-only sliding windows over `&[f64]`, and window sizes are small.
fn median_of(slice: &[f64]) -> f64 {
    debug_assert!(!slice.is_empty());
    let mut v = slice.to_vec();
    let mid = v.len() / 2;
    let by_total = |a: &f64, b: &f64| a.total_cmp(b);
    if v.len() % 2 == 1 {
        *v.select_nth_unstable_by(mid, by_total).1
    } else {
        let lower = *v.select_nth_unstable_by(mid - 1, by_total).1;
        let upper = *v.select_nth_unstable_by(mid, by_total).1;
        (lower + upper) / 2.0
    }
}

/// Centered moving average with window `k` (rounded up to odd), shrunk at the
/// edges: $y_i = \frac{1}{|W_i|}\sum_{j \in W_i} x_j$.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::moving_average;
///
/// let x = [0.0, 0.0, 9.0, 0.0, 0.0];
/// let y = moving_average(&x, 3).unwrap();
/// assert!((y[2] - 3.0).abs() < 1e-12); // (0 + 9 + 0) / 3
/// assert!((y[0] - 0.0).abs() < 1e-12); // shrunk window at the edge
/// ```
pub fn moving_average(x: &[f64], k: usize) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    let k = odd_window(k)?;
    let n = x.len();
    // Prefix sums make this O(n) instead of O(nk).
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0);
    for &v in x {
        prefix.push(prefix.last().unwrap() + v);
    }
    Ok((0..n)
        .map(|i| {
            let (lo, hi) = window_bounds(n, i, k);
            (prefix[hi] - prefix[lo]) / (hi - lo) as f64
        })
        .collect())
}

/// Exponentially weighted moving average:
/// $y_i = \alpha x_i + (1-\alpha) y_{i-1}$, $\alpha = 2/(k+1)$.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::exp_moving_average;
///
/// let x = [1.0, 1.0, 1.0, 1.0];
/// let y = exp_moving_average(&x, 3).unwrap(); // alpha = 0.5
/// assert!((y[0] - 1.0).abs() < 1e-12);
/// assert!((y[3] - 1.0).abs() < 1e-12);
/// let z = exp_moving_average(&[0.0, 2.0], 1).unwrap(); // alpha = 1 -> identity
/// assert_eq!(z, vec![0.0, 2.0]);
/// ```
pub fn exp_moving_average(x: &[f64], k: usize) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    if k == 0 {
        return Err(DspError::InvalidConfig("window size k must be >= 1".to_string()));
    }
    let alpha = 2.0 / (k as f64 + 1.0);
    let mut y = Vec::with_capacity(x.len());
    let mut acc = x[0];
    y.push(acc);
    for &v in &x[1..] {
        acc = alpha * v + (1.0 - alpha) * acc;
        y.push(acc);
    }
    Ok(y)
}

/// Centered median filter with window `k` (rounded up to odd), shrunk at the
/// edges. Removes impulse noise while preserving edges better than a moving
/// average.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::median_filter;
///
/// let x = [1.0, 1.0, 100.0, 1.0, 1.0];
/// let y = median_filter(&x, 3).unwrap();
/// assert_eq!(y[2], 1.0); // spike removed
/// ```
pub fn median_filter(x: &[f64], k: usize) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    let k = odd_window(k)?;
    let n = x.len();
    Ok((0..n)
        .map(|i| {
            let (lo, hi) = window_bounds(n, i, k);
            median_of(&x[lo..hi])
        })
        .collect())
}

/// Hampel filter: replace samples that deviate from the local median by more
/// than `n_sigma` robust standard deviations.
///
/// For each sample $i$ with centered window $W_i$ (size `window`, rounded up
/// to odd, shrunk at the edges):
///
/// $$m_i = \operatorname{median}(W_i), \qquad
///    \sigma_i = 1.4826 \cdot \operatorname{median}_{j \in W_i} |x_j - m_i|$$
///
/// and $x_i$ is replaced by $m_i$ when $|x_i - m_i| > n_\sigma\, \sigma_i$.
/// When $\sigma_i = 0$ (constant window) the sample is kept.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::hampel_filter;
///
/// let mut x: Vec<f64> = (0..50).map(|i| (i as f64 * 0.4).sin()).collect();
/// x[10] += 50.0; // injected spike
/// x[30] -= 50.0;
/// let y = hampel_filter(&x, 7, 3.0).unwrap();
/// // Spikes are replaced by the window median, which tracks the smooth
/// // sine to within its curvature over the window.
/// assert!((y[10] - (10.0f64 * 0.4).sin()).abs() < 0.35);
/// assert!((y[30] - (30.0f64 * 0.4).sin()).abs() < 0.35);
/// ```
pub fn hampel_filter(x: &[f64], window: usize, n_sigma: f64) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    if !n_sigma.is_finite() || n_sigma <= 0.0 {
        return Err(DspError::InvalidConfig(format!(
            "n_sigma must be positive and finite, got {n_sigma}"
        )));
    }
    let k = odd_window(window)?;
    let n = x.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (lo, hi) = window_bounds(n, i, k);
        let w = &x[lo..hi];
        let med = median_of(w);
        let mad: Vec<f64> = w.iter().map(|v| (v - med).abs()).collect();
        let sigma = 1.4826 * median_of(&mad);
        if sigma > 0.0 && (x[i] - med).abs() > n_sigma * sigma {
            out.push(med);
        } else {
            out.push(x[i]);
        }
    }
    Ok(out)
}

/// Run one RBJ biquad (Butterworth Q) over `x`.
fn run_biquad(
    x: &[f64],
    fs: f64,
    filter_type: Type<f64>,
    f0_hz: f64,
) -> Result<Vec<f64>, DspError> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err(DspError::InvalidConfig(format!(
            "fs must be positive and finite, got {fs}"
        )));
    }
    if !f0_hz.is_finite() || f0_hz <= 0.0 || f0_hz >= fs / 2.0 {
        return Err(DspError::InvalidConfig(format!(
            "cutoff {f0_hz} Hz must be in (0, fs/2 = {} Hz)",
            fs / 2.0
        )));
    }
    let fs_hz = Hertz::from_hz(fs)
        .map_err(|e| DspError::InvalidConfig(format!("bad fs {fs}: {e:?}")))?;
    let f0 = Hertz::from_hz(f0_hz)
        .map_err(|e| DspError::InvalidConfig(format!("bad cutoff {f0_hz}: {e:?}")))?;
    let coeffs = Coefficients::<f64>::from_params(filter_type, fs_hz, f0, Q_BUTTERWORTH_F64)
        .map_err(|e| DspError::InvalidConfig(format!("biquad coefficients: {e:?}")))?;
    let mut biquad = DirectForm2Transposed::<f64>::new(coeffs);
    Ok(x.iter().map(|&v| biquad.run(v)).collect())
}

/// Second-order Butterworth low-pass filter with cutoff `cutoff_hz`.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::{fft, lowpass, Window, dominant_peaks};
///
/// let fs = 100.0;
/// let n = 1000;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| {
///         let t = i as f64 / fs;
///         (2.0 * std::f64::consts::PI * 1.0 * t).sin()
///             + (2.0 * std::f64::consts::PI * 30.0 * t).sin()
///     })
///     .collect();
/// let filtered = lowpass(&signal, fs, 5.0).unwrap();
/// let spec = fft(&filtered[100..], fs, Window::Hann).unwrap(); // skip transient
/// let peaks = dominant_peaks(&spec, 0.0, fs / 2.0, 3);
/// assert!((peaks[0].0 - 1.0).abs() <= fs / (n - 100) as f64 + 1e-9);
/// ```
pub fn lowpass(x: &[f64], fs: f64, cutoff_hz: f64) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    run_biquad(x, fs, Type::LowPass, cutoff_hz)
}

/// Second-order Butterworth high-pass filter with cutoff `cutoff_hz`.
/// Removes DC and slow drift (static-object reflections).
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::highpass;
///
/// let fs = 50.0;
/// let n = 500;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| 10.0 + (2.0 * std::f64::consts::PI * 2.0 * i as f64 / fs).sin())
///     .collect();
/// let filtered = highpass(&signal, fs, 0.5).unwrap();
/// // After the transient, the DC offset is gone.
/// let tail = &filtered[200..];
/// let mean = tail.iter().sum::<f64>() / tail.len() as f64;
/// assert!(mean.abs() < 0.1);
/// ```
pub fn highpass(x: &[f64], fs: f64, cutoff_hz: f64) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    run_biquad(x, fs, Type::HighPass, cutoff_hz)
}

/// Band-pass filter keeping `[lo_hz, hi_hz]`: a high-pass at `lo_hz` cascaded
/// with a low-pass at `hi_hz`.
///
/// Typical use: 0.1–0.5 Hz to isolate breathing.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::{bandpass, fft, Window, dominant_peaks};
///
/// let fs = 50.0;
/// let n = 2000;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| {
///         let t = i as f64 / fs;
///         (2.0 * std::f64::consts::PI * 0.3 * t).sin()   // in band
///             + (2.0 * std::f64::consts::PI * 8.0 * t).sin() // out of band
///     })
///     .collect();
/// let filtered = bandpass(&signal, fs, 0.1, 0.5).unwrap();
/// let spec = fft(&filtered[500..], fs, Window::Hann).unwrap();
/// let peaks = dominant_peaks(&spec, 0.0, fs / 2.0, 3);
/// assert!((peaks[0].0 - 0.3).abs() <= fs / (n - 500) as f64 + 1e-9);
/// ```
pub fn bandpass(x: &[f64], fs: f64, lo_hz: f64, hi_hz: f64) -> Result<Vec<f64>, DspError> {
    check_signal(x)?;
    if lo_hz.partial_cmp(&hi_hz) != Some(std::cmp::Ordering::Less) {
        return Err(DspError::InvalidConfig(format!(
            "bandpass needs lo_hz < hi_hz, got {lo_hz} >= {hi_hz}"
        )));
    }
    let y = highpass(x, fs, lo_hz)?;
    lowpass(&y, fs, hi_hz)
}

impl CsiSignal {
    /// Centered moving average of every amplitude subcarrier (see
    /// [`moving_average`]).
    pub fn moving_average(&self, k: usize) -> Result<Self, DspError> {
        self.map_amplitude_columns(|col| moving_average(col, k))
    }

    /// Exponentially weighted moving average of every amplitude subcarrier
    /// (see [`exp_moving_average`]).
    pub fn exp_moving_average(&self, k: usize) -> Result<Self, DspError> {
        self.map_amplitude_columns(|col| exp_moving_average(col, k))
    }

    /// Median filter of every amplitude subcarrier (see [`median_filter`]).
    pub fn median_filter(&self, k: usize) -> Result<Self, DspError> {
        self.map_amplitude_columns(|col| median_filter(col, k))
    }

    /// Hampel filter of every amplitude subcarrier (see [`hampel_filter`]).
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let fs = 20.0;
    /// let n = 200;
    /// let mut iq = Array2::from_shape_fn((n, 1), |(i, _)| {
    ///     let t = i as f64 / fs;
    ///     Complex::new((2.0 * std::f64::consts::PI * 0.3 * t).sin(), 0.0)
    /// });
    /// iq[[50, 0]] += Complex::new(100.0, 0.0); // impulse noise
    /// let ticks = Array1::from_iter((0..n).map(|i| (i as f64 / fs * 1e6) as u64));
    /// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
    /// let clean = signal.hampel(7, 3.0).unwrap();
    /// let t = 50.0 / fs;
    /// // amplitude is a magnitude; the spike is replaced by the window median.
    /// let expected = (2.0 * std::f64::consts::PI * 0.3 * t).sin().abs();
    /// assert!((clean.amplitude[[50, 0]] - expected).abs() < 0.35);
    /// ```
    pub fn hampel(&self, window: usize, n_sigma: f64) -> Result<Self, DspError> {
        self.map_amplitude_columns(|col| hampel_filter(col, window, n_sigma))
    }

    /// Low-pass every amplitude subcarrier (see [`lowpass`]).
    pub fn lowpass(&self, cutoff_hz: f64) -> Result<Self, DspError> {
        let fs = self.fs;
        self.map_amplitude_columns(|col| lowpass(col, fs, cutoff_hz))
    }

    /// High-pass every amplitude subcarrier (see [`highpass`]).
    pub fn highpass(&self, cutoff_hz: f64) -> Result<Self, DspError> {
        let fs = self.fs;
        self.map_amplitude_columns(|col| highpass(col, fs, cutoff_hz))
    }

    /// Band-pass every amplitude subcarrier to `[lo_hz, hi_hz]` (see
    /// [`bandpass`]).
    pub fn bandpass(&self, lo_hz: f64, hi_hz: f64) -> Result<Self, DspError> {
        let fs = self.fs;
        self.map_amplitude_columns(|col| bandpass(col, fs, lo_hz, hi_hz))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, fs: f64, freq: f64, amp: f64) -> Vec<f64> {
        (0..n)
            .map(|i| amp * (2.0 * std::f64::consts::PI * freq * i as f64 / fs).sin())
            .collect()
    }

    fn peak_hz(x: &[f64], fs: f64) -> (f64, f64) {
        let spec = super::super::spectral::fft(x, fs, super::super::spectral::Window::Hann).unwrap();
        let (best, &mag) = spec
            .magnitude
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        (spec.freqs[best], mag)
    }

    #[test]
    fn moving_average_smooths_and_handles_edges() {
        let x = [0.0, 0.0, 9.0, 0.0, 0.0];
        let y = moving_average(&x, 3).unwrap();
        assert!((y[2] - 3.0).abs() < 1e-12);
        assert!((y[0] - 0.0).abs() < 1e-12); // shrunk edge window: mean of [0, 0]
        assert!((y[4] - 0.0).abs() < 1e-12); // shrunk edge window: mean of [0, 0]
        // even k rounds up to odd
        let y2 = moving_average(&x, 2).unwrap();
        assert!((y2[2] - 3.0).abs() < 1e-12);
        assert!(matches!(moving_average(&[], 3), Err(DspError::EmptySignal)));
        assert!(matches!(moving_average(&x, 0), Err(DspError::InvalidConfig(_))));
    }

    #[test]
    fn exp_moving_average_matches_recursion() {
        let x: Vec<f64> = (0..20).map(|i| (i as f64).sin()).collect();
        let y = exp_moving_average(&x, 5).unwrap();
        let alpha = 2.0 / 6.0;
        let mut acc = x[0];
        assert!((y[0] - acc).abs() < 1e-12);
        for i in 1..x.len() {
            acc = alpha * x[i] + (1.0 - alpha) * acc;
            assert!((y[i] - acc).abs() < 1e-12);
        }
    }

    #[test]
    fn median_filter_removes_impulse_noise() {
        let mut x = sine(200, 50.0, 1.0, 1.0);
        x[57] += 25.0;
        x[120] -= 25.0;
        let y = median_filter(&x, 5).unwrap();
        let clean = sine(200, 50.0, 1.0, 1.0);
        for i in 0..x.len() {
            if (i as i64 - 57).abs() <= 2 || (i as i64 - 120).abs() <= 2 {
                assert!((y[i] - clean[i]).abs() < 0.2, "sample {i}");
            }
        }
    }

    #[test]
    fn hampel_recovers_clean_signal_from_spikes() {
        let fs = 50.0;
        let n = 400;
        let clean = sine(n, fs, 0.8, 1.0);
        let mut x = clean.clone();
        for &idx in &[31usize, 200, 355] {
            x[idx] += 40.0;
        }
        let y = hampel_filter(&x, 9, 3.0).unwrap();
        let mut max_err = 0.0f64;
        for i in 0..n {
            max_err = max_err.max((y[i] - clean[i]).abs());
        }
        // Replaced samples become the window median, which tracks the smooth
        // sine to within its curvature over the 9-sample window.
        assert!(max_err < 0.3, "max err {max_err}");
        // n_sigma validation
        assert!(matches!(
            hampel_filter(&x, 5, 0.0),
            Err(DspError::InvalidConfig(_))
        ));
    }

    #[test]
    fn lowpass_attenuates_high_frequency_interference() {
        let fs = 100.0;
        let n = 2000;
        let signal: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                (2.0 * std::f64::consts::PI * 1.0 * t).sin()
                    + (2.0 * std::f64::consts::PI * 30.0 * t).sin()
            })
            .collect();
        let filtered = lowpass(&signal, fs, 5.0).unwrap();
        let tail = &filtered[500..]; // skip biquad transient
        let (peak, mag) = peak_hz(tail, fs);
        let m = tail.len() as f64;
        assert!((peak - 1.0).abs() <= fs / m + 1e-9);
        assert!(mag > 0.9, "in-band magnitude {mag}");
        // The 30 Hz component is attenuated by > 40 dB relative to the 1 Hz one.
        let spec = super::super::spectral::fft(tail, fs, super::super::spectral::Window::Hann).unwrap();
        let at_30 = spec
            .magnitude
            .iter()
            .zip(spec.freqs.iter())
            .filter(|(_, f)| (**f - 30.0).abs() <= fs / m)
            .map(|(m_, _)| *m_)
            .fold(0.0f64, f64::max);
        // A 2nd-order Butterworth LPF at 5 Hz attenuates 30 Hz by ~36 dB.
        assert!(at_30 < mag / 50.0, "30 Hz residual {at_30} vs peak {mag}");
    }

    #[test]
    fn highpass_removes_low_frequency_and_dc() {
        let fs = 50.0;
        let n = 1000;
        let signal: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                5.0 + 0.2 * (2.0 * std::f64::consts::PI * 0.1 * t).sin() // DC + very slow
                    + (2.0 * std::f64::consts::PI * 5.0 * t).sin()
            })
            .collect();
        let filtered = highpass(&signal, fs, 1.0).unwrap();
        let tail = &filtered[400..];
        let mean = tail.iter().sum::<f64>() / tail.len() as f64;
        assert!(mean.abs() < 0.05, "mean after HPF {mean}");
        let (peak, _) = peak_hz(tail, fs);
        assert!((peak - 5.0).abs() <= fs / tail.len() as f64 + 1e-9);
    }

    #[test]
    fn bandpass_passes_in_band_and_rejects_out_of_band() {
        let fs = 50.0;
        let n = 4000; // 80 s for 0.3 Hz resolution
        let signal: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                (2.0 * std::f64::consts::PI * 0.3 * t).sin() // in band
                    + (2.0 * std::f64::consts::PI * 2.0 * t).sin() // above band
                    + 0.5 // DC
            })
            .collect();
        let filtered = bandpass(&signal, fs, 0.1, 0.5).unwrap();
        let tail = &filtered[1000..];
        let spec = super::super::spectral::fft(tail, fs, super::super::spectral::Window::Hann).unwrap();
        let df = fs / tail.len() as f64;
        let mag_at = |target: f64| {
            spec.magnitude
                .iter()
                .zip(spec.freqs.iter())
                .filter(|(_, f)| (**f - target).abs() <= df)
                .map(|(m, _)| *m)
                .fold(0.0f64, f64::max)
        };
        let in_band = mag_at(0.3);
        let out_band = mag_at(2.0);
        let dc = spec.magnitude[0]; // bin 1 (~0.0125 Hz), DC leakage region
        assert!(in_band > 0.8, "in-band {in_band}");
        // LPF half at 0.5 Hz attenuates 2 Hz (4x cutoff) by ~24 dB.
        assert!(out_band < in_band / 10.0, "out-of-band {out_band} vs {in_band}");
        assert!(dc < in_band / 10.0, "dc {dc} vs {in_band}");
        // invalid band
        assert!(matches!(
            bandpass(&signal, fs, 0.5, 0.1),
            Err(DspError::InvalidConfig(_))
        ));
        assert!(matches!(
            bandpass(&signal, fs, 0.1, 100.0), // hi >= fs/2
            Err(DspError::InvalidConfig(_))
        ));
    }

    #[test]
    fn csi_signal_filters_apply_column_wise() {
        use ndarray::{Array1, Array2};
        use rustfft::num_complex::Complex;

        let fs = 20.0;
        let n = 200;
        // Positive amplitude with a tone on top of a DC offset, so amplitude
        // keeps the tone's frequency (no full-wave-rectification folding).
        let mut iq = Array2::from_shape_fn((n, 2), |(i, k)| {
            let t = i as f64 / fs;
            let f = if k == 0 { 0.3 } else { 2.0 };
            let mut v = 5.0 + (2.0 * std::f64::consts::PI * f * t).sin();
            if i == 100 {
                v += 50.0; // spike on both subcarriers
            }
            Complex::new(v, 0.0)
        });
        let _ = &mut iq;
        let ticks = Array1::from_iter((0..n).map(|i| (i as f64 / fs * 1e6) as u64));
        let signal = CsiSignal::from_iq(iq, ticks).unwrap();

        let clean = signal.hampel(7, 3.0).unwrap();
        assert_eq!(clean.n_samples(), n);
        assert_eq!(clean.n_subcarriers(), 2);
        // The +50 spike at i=100 is replaced by the local median (~5 + tone).
        let expected = 5.0 + (2.0 * std::f64::consts::PI * 0.3 * 100.0 / fs).sin();
        assert!(
            (clean.amplitude[[100, 0]] - expected).abs() < 0.5,
            "spike not removed: {}",
            clean.amplitude[[100, 0]]
        );

        // Compare bandpass tail energies against the cleaned signal (the
        // spike would dominate the input energy; the first half is the biquad
        // startup transient of the DC offset).
        let bp = clean.bandpass(0.1, 0.5).unwrap();
        assert_eq!(bp.amplitude.dim(), clean.amplitude.dim());
        use ndarray::s;
        // AC energy of the tail (mean removed): the bandpass strips the DC
        // offset by design, so only the tone energy is comparable.
        let tail_ac_e = |a: &ndarray::Array2<f64>, k: usize| {
            let col = a.slice(s![150.., k]);
            let mean = col.mean().unwrap();
            col.mapv(|v| (v - mean).powi(2)).sum()
        };
        // subcarrier 0 (0.3 Hz) is in band; subcarrier 1 (2 Hz) is not
        let r0 = tail_ac_e(&bp.amplitude, 0) / tail_ac_e(&clean.amplitude, 0);
        let r1 = tail_ac_e(&bp.amplitude, 1) / tail_ac_e(&clean.amplitude, 1);
        assert!(r0 > 0.5, "in-band energy kept: ratio {r0}");
        assert!(r1 < 0.2, "out-of-band energy removed: ratio {r1}");
    }
}
