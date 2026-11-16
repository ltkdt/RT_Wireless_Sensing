//! Spectral analysis: windowing, FFT, dominant peaks, and STFT.
//!
//! All transforms use the `realfft` crate (a real-input FFT around `rustfft`)
//! so the one-sided spectrum comes out directly and about twice as fast as a
//! complex FFT. For a real signal of length $N$ sampled at $f_s$, the
//! one-sided DFT is
//!
//! $$X_k = \sum_{n=0}^{N-1} w_n\, x_n\, e^{-j 2\pi k n / N}, \qquad
//!    f_k = \frac{k f_s}{N}, \quad k = 0 \dots \lfloor N/2 \rfloor$$
//!
//! [`fft`] removes the mean (DC) first, applies the window $w_n$, and reports
//! bins $k = 1 \dots \lfloor N/2 \rfloor$ with amplitude scaling
//!
//! $$|X_k| \mapsto \frac{2\,|X_k|}{\sum_n w_n}$$
//!
//! (the Nyquist bin $k = N/2$ for even $N$ is not doubled). With a Hann window
//! $\sum w_n \approx N/2$, so a pure sine of amplitude $A$ shows a peak of
//! height $\approx A$.
//!
//! [`stft`] slides a window over the signal with a hop and stacks the
//! one-sided spectra of all frames into a [`Spectrogram`].

use std::sync::Arc;

use ndarray::{Array1, Array2};
use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex;

use super::{CsiSignal, DspError};

/// Window function applied before the FFT/STFT.
///
/// All windows are *periodic* (denominator $N$, not $N-1$), which is the
/// right choice for DFT spectral analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    /// Rectangular window $w_n = 1$. Maximum leakage, maximum resolution.
    None,
    /// Hann window $w_n = 0.5 - 0.5\cos(2\pi n / N)$. Default; good
    /// frequency resolution with moderate leakage.
    #[default]
    Hann,
    /// Hamming window $w_n = 0.54 - 0.46\cos(2\pi n / N)$.
    Hamming,
    /// Blackman window $w_n = 0.42 - 0.5\cos(2\pi n/N) + 0.08\cos(4\pi n/N)$.
    /// Lowest leakage, widest main lobe.
    Blackman,
}

impl Window {
    /// Window coefficients $w_n$ for a window of length `n`.
    pub fn coeffs(&self, n: usize) -> Array1<f64> {
        Array1::from_iter((0..n).map(|i| {
            let x = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            match self {
                Window::None => 1.0,
                Window::Hann => 0.5 - 0.5 * x.cos(),
                Window::Hamming => 0.54 - 0.46 * x.cos(),
                Window::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
            }
        }))
    }

    /// Sum of the window coefficients, $\sum_n w_n$, used for amplitude
    /// scaling so a sine of amplitude $A$ peaks at $\approx A$.
    pub fn sum(&self, n: usize) -> f64 {
        self.coeffs(n).sum()
    }
}

/// One-sided amplitude spectrum of a real signal.
///
/// `freqs[k] = (k+1) * fs / n` for bin `k+1` (DC bin 0 is dropped), and
/// `magnitude` is amplitude-scaled ($2|X|/\sum w$, Nyquist bin not doubled).
/// `complex` holds the unscaled windowed DFT coefficients for the same bins.
#[derive(Debug, Clone)]
pub struct Spectrum {
    /// Frequency of each bin in Hz, length `n/2` (bins 1..=n/2).
    pub freqs: Array1<f64>,
    /// Amplitude-scaled magnitude per bin.
    pub magnitude: Array1<f64>,
    /// Raw windowed DFT coefficients per bin.
    pub complex: Array1<Complex<f64>>,
}

/// One-sided frequency bins $f_k = k f_s / N$ for $k = 0 \dots \lfloor N/2
/// \rfloor$ (DC and, for even `n`, Nyquist included).
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::freq_bins;
///
/// let bins = freq_bins(8, 100.0);
/// assert_eq!(bins.as_slice().unwrap(), &[0.0, 12.5, 25.0, 37.5, 50.0]);
/// ```
pub fn freq_bins(n: usize, fs: f64) -> Array1<f64> {
    let half = n / 2;
    Array1::from_iter((0..=half).map(|k| k as f64 * fs / n as f64))
}

/// Compute the one-sided amplitude spectrum of `signal`.
///
/// The mean is removed (DC), the window is applied, and the real FFT produces
/// bins $k = 1 \dots \lfloor n/2 \rfloor$ (DC excluded). Magnitudes are scaled
/// by $2 / \sum_n w_n$ so that a pure sine of amplitude $A$ at a bin center
/// reads $\approx A$; the Nyquist bin of an even-length signal is scaled by
/// $1 / \sum_n w_n$ instead.
///
/// # Errors
///
/// [`DspError::EmptySignal`] for an empty signal, [`DspError::InvalidConfig`]
/// when `fs` is not positive and finite or the signal has fewer than 2 samples.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::{dominant_peaks, fft, Window};
///
/// let fs = 100.0;
/// let n = 1000;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| 3.0 * (2.0 * std::f64::consts::PI * 5.0 * i as f64 / fs).sin())
///     .collect();
/// let spectrum = fft(&signal, fs, Window::Hann).unwrap();
/// let peaks = dominant_peaks(&spectrum, 0.0, fs / 2.0, 1);
/// let (peak_hz, peak_mag) = peaks[0];
/// assert!((peak_hz - 5.0).abs() <= fs / n as f64); // within one bin
/// assert!((peak_mag - 3.0).abs() < 0.05);          // amplitude recovered
/// ```
pub fn fft(signal: &[f64], fs: f64, window: Window) -> Result<Spectrum, DspError> {
    let n = signal.len();
    if n == 0 {
        return Err(DspError::EmptySignal);
    }
    if n < 2 {
        return Err(DspError::InvalidConfig(
            "fft needs at least 2 samples".to_string(),
        ));
    }
    if !fs.is_finite() || fs <= 0.0 {
        return Err(DspError::InvalidConfig(format!(
            "fs must be positive and finite, got {fs}"
        )));
    }

    let w = window.coeffs(n);
    let wsum = w.sum();
    let mean = signal.iter().sum::<f64>() / n as f64;
    let mut buf: Vec<f64> = signal
        .iter()
        .zip(w.iter())
        .map(|(&x, &wi)| (x - mean) * wi)
        .collect();

    let mut planner = RealFftPlanner::<f64>::new();
    let engine = planner.plan_fft_forward(n);
    let mut out = engine.make_output_vec();
    let mut scratch = engine.make_scratch_vec();
    engine
        .process_with_scratch(&mut buf, &mut out, &mut scratch)
        .map_err(|e| DspError::InvalidConfig(format!("real FFT failed: {e}")))?;

    Ok(one_sided(&out, n, fs, wsum))
}

/// Build the one-sided [`Spectrum`] from a full real-FFT output, dropping DC.
fn one_sided(out: &[Complex<f64>], n: usize, fs: f64, wsum: f64) -> Spectrum {
    let half = n / 2;
    let mut freqs = Array1::zeros(half);
    let mut magnitude = Array1::zeros(half);
    let mut complex = Array1::zeros(half);
    for k in 1..=half {
        let scale = if n.is_multiple_of(2) && k == half { 1.0 } else { 2.0 } / wsum;
        freqs[k - 1] = k as f64 * fs / n as f64;
        magnitude[k - 1] = out[k].norm() * scale;
        complex[k - 1] = out[k];
    }
    Spectrum {
        freqs,
        magnitude,
        complex,
    }
}

/// Dominant spectral peaks of `spectrum` within `[min_hz, max_hz]`.
///
/// Returns up to `top_k` `(frequency, magnitude)` pairs sorted by descending
/// magnitude. This replaces the old hardcoded 0.2–10 Hz band: the band is
/// caller-specified, e.g. `0.1–0.5` Hz for breathing, `0.2–10` Hz for motion.
/// An empty or inverted band yields an empty vector.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::{dominant_peaks, fft, Window};
///
/// let fs = 50.0;
/// let n = 1000;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| {
///         let t = i as f64 / fs;
///         (2.0 * std::f64::consts::PI * 0.3 * t).sin()
///             + 0.5 * (2.0 * std::f64::consts::PI * 4.0 * t).sin()
///     })
///     .collect();
/// let spectrum = fft(&signal, fs, Window::Hann).unwrap();
/// // Breathing band only: the 4 Hz component is excluded, so the strongest
/// // peak is the 0.3 Hz tone.
/// let peaks = dominant_peaks(&spectrum, 0.1, 0.5, 5);
/// assert!((peaks[0].0 - 0.3).abs() <= fs / n as f64);
/// assert!((peaks[0].1 - 1.0).abs() < 0.05);
/// ```
pub fn dominant_peaks(
    spectrum: &Spectrum,
    min_hz: f64,
    max_hz: f64,
    top_k: usize,
) -> Vec<(f64, f64)> {
    let mut peaks: Vec<(f64, f64)> = spectrum
        .freqs
        .iter()
        .zip(spectrum.magnitude.iter())
        .filter(|(f, _)| **f >= min_hz && **f <= max_hz)
        .map(|(f, m)| (*f, *m))
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks.truncate(top_k);
    peaks
}

impl CsiSignal {
    /// One-sided spectrum of every amplitude subcarrier.
    ///
    /// Uses [`fft`] with the signal's `fs` on each column of `amplitude`.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::{CsiSignal, Window};
    ///
    /// let fs = 20.0;
    /// let n = 400;
    /// let iq = Array2::from_shape_fn((n, 2), |(i, k)| {
    ///     let f = if k == 0 { 0.3 } else { 2.0 };
    ///     let t = i as f64 / fs;
    ///     // DC offset keeps the amplitude positive so the tone stays at f Hz.
    ///     Complex::new(5.0 + (2.0 * std::f64::consts::PI * f * t).sin(), 0.0)
    /// });
    /// let ticks = Array1::from_iter((0..n).map(|i| (i as f64 / fs * 1e6) as u64));
    /// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
    /// let spectra = signal.fft_subcarriers(Window::Hann).unwrap();
    /// assert_eq!(spectra.len(), 2);
    /// let top0 = spectra[0].magnitude.iter().enumerate()
    ///     .max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    /// assert!((spectra[0].freqs[top0.0] - 0.3).abs() <= fs / n as f64);
    /// ```
    pub fn fft_subcarriers(&self, window: Window) -> Result<Vec<Spectrum>, DspError> {
        let mut spectra = Vec::with_capacity(self.n_subcarriers());
        for k in 0..self.n_subcarriers() {
            let col = self.column(k);
            spectra.push(fft(col.as_slice().expect("owned column is contiguous"), self.fs, window)?);
        }
        Ok(spectra)
    }
}

/// Configuration for [`stft`].
///
/// Defaults: Hann window, 50% overlap (`hop = window_len / 2`), and a neutral
/// `window_len` of 256 samples. Choose the window by physics, not by habit:
///
/// | Signal of interest | Window length | At 20 Hz sampling |
/// |---|---|---|
/// | Breathing (0.1–0.5 Hz) | 10–20 s | 200–400 samples |
/// | Motion / gestures (0.2–10 Hz) | 0.5–1 s | 10–20 samples |
///
/// Long windows give good frequency resolution but poor time resolution, and
/// vice versa.
#[derive(Debug, Clone, PartialEq)]
pub struct StftConfig {
    /// Window length in samples; must be at least 2.
    pub window_len: usize,
    /// Hop between consecutive frame starts, in samples; `1 <= hop <=
    /// window_len`. `window_len / 2` means 50% overlap.
    pub hop: usize,
    /// Window applied to each frame.
    pub window: Window,
}

impl Default for StftConfig {
    fn default() -> Self {
        Self {
            window_len: 256,
            hop: 128,
            window: Window::Hann,
        }
    }
}

impl StftConfig {
    /// Build a config, validating `window_len >= 2` and `1 <= hop <=
    /// window_len`.
    pub fn new(window_len: usize, hop: usize, window: Window) -> Result<Self, DspError> {
        if window_len < 2 {
            return Err(DspError::InvalidConfig(format!(
                "window_len must be >= 2, got {window_len}"
            )));
        }
        if hop == 0 || hop > window_len {
            return Err(DspError::InvalidConfig(format!(
                "hop must be in 1..=window_len ({window_len}), got {hop}"
            )));
        }
        Ok(Self {
            window_len,
            hop,
            window,
        })
    }

    /// Build a config with 50% overlap: `hop = window_len / 2`.
    ///
    /// # Example
    ///
    /// ```
    /// use rt_wireless_sensing::dsp::{StftConfig, Window};
    ///
    /// let config = StftConfig::with_window(400, Window::Hann).unwrap();
    /// assert_eq!(config.hop, 200);
    /// assert!(StftConfig::new(1, 1, Window::Hann).is_err());
    /// ```
    pub fn with_window(window_len: usize, window: Window) -> Result<Self, DspError> {
        Self::new(window_len, (window_len / 2).max(1), window)
    }
}

/// Short-time Fourier transform result: a spectrogram.
///
/// `magnitudes` has shape `(n_freqs, n_frames)` — frequency × time frame — with
/// `freqs[k] = (k+1) * fs / window_len` (DC excluded) and
/// `times[j] = (start_j + window_len/2) / fs`, the center time of frame `j` in
/// seconds. Magnitudes use the same amplitude scaling as [`fft`]
/// ($2|X|/\sum w$, Nyquist bin not doubled).
#[derive(Debug, Clone)]
pub struct Spectrogram {
    /// Center time of each frame in seconds, length `n_frames`.
    pub times: Array1<f64>,
    /// Frequency of each row in Hz, length `n_freqs`.
    pub freqs: Array1<f64>,
    /// Magnitudes, shape `(n_freqs, n_frames)`.
    pub magnitudes: Array2<f64>,
}

impl Spectrogram {
    /// Return a copy with magnitudes converted to decibels:
    /// $20\log_{10}(\max(m, 10^{-12}))$.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rt_wireless_sensing::dsp::Spectrogram;
    ///
    /// let spec = Spectrogram {
    ///     times: Array1::from_vec(vec![0.5, 1.0]),
    ///     freqs: Array1::from_vec(vec![1.0]),
    ///     magnitudes: Array2::from_shape_vec((1, 2), vec![1.0, 0.01]).unwrap(),
    /// };
    /// let db = spec.to_db();
    /// assert!((db.magnitudes[[0, 0]] - 0.0).abs() < 1e-9);   // 1.0 -> 0 dB
    /// assert!((db.magnitudes[[0, 1]] + 40.0).abs() < 1e-9);  // 0.01 -> -40 dB
    /// ```
    pub fn to_db(&self) -> Self {
        Self {
            times: self.times.clone(),
            freqs: self.freqs.clone(),
            magnitudes: self.magnitudes.mapv(|m| 20.0 * m.max(1e-12).log10()),
        }
    }
}

/// Short-time Fourier transform of `signal`.
///
/// Frames start at `0, hop, 2*hop, …` while a full window fits; each frame has
/// its mean removed, is windowed, and transformed with one planned real FFT
/// reused across all frames. Output bins are $k = 1 \dots \lfloor
/// window\_len/2 \rfloor$.
///
/// # Errors
///
/// [`DspError::EmptySignal`] / [`DspError::InvalidConfig`] for bad `fs`, an
/// empty signal, or a signal shorter than `window_len`.
///
/// # Example
///
/// ```
/// use rt_wireless_sensing::dsp::{StftConfig, Window, stft};
///
/// let fs = 100.0;
/// let n = 1000;
/// let signal: Vec<f64> = (0..n)
///     .map(|i| (2.0 * std::f64::consts::PI * 4.0 * i as f64 / fs).sin())
///     .collect();
/// let config = StftConfig::new(200, 100, Window::Hann).unwrap();
/// let spec = stft(&signal, fs, &config).unwrap();
/// // 9 frames: starts 0,100,...,800; 100 freq bins (bins 1..=100).
/// assert_eq!(spec.times.len(), 9);
/// assert_eq!(spec.magnitudes.dim(), (100, 9));
/// // Every frame peaks at 4 Hz.
/// for j in 0..spec.times.len() {
///     let col = spec.magnitudes.column(j);
///     let (best, _) = col.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
///     assert!((spec.freqs[best] - 4.0).abs() <= fs / 200.0);
/// }
/// ```
pub fn stft(signal: &[f64], fs: f64, config: &StftConfig) -> Result<Spectrogram, DspError> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err(DspError::InvalidConfig(format!(
            "fs must be positive and finite, got {fs}"
        )));
    }
    let n = signal.len();
    if n == 0 {
        return Err(DspError::EmptySignal);
    }
    let w = config.window_len;
    if n < w {
        return Err(DspError::InvalidConfig(format!(
            "signal has {n} samples, shorter than window_len {w}"
        )));
    }

    let window = config.window.coeffs(w);
    let wsum = window.sum();

    let n_frames = (n - w) / config.hop + 1;
    let half = w / 2;
    let n_freqs = half; // bins 1..=half

    let mut planner = RealFftPlanner::<f64>::new();
    let engine: Arc<dyn RealToComplex<f64>> = planner.plan_fft_forward(w);
    let mut scratch = engine.make_scratch_vec();
    let mut out = engine.make_output_vec();

    let mut times = Array1::zeros(n_frames);
    let mut freqs = Array1::zeros(n_freqs);
    let mut magnitudes = Array2::<f64>::zeros((n_freqs, n_frames));

    for k in 1..=half {
        freqs[k - 1] = k as f64 * fs / w as f64;
    }

    for (j, start) in (0..n_frames).map(|j| j * config.hop).enumerate() {
        let frame = &signal[start..start + w];
        let mean = frame.iter().sum::<f64>() / w as f64;
        let mut buf: Vec<f64> = frame
            .iter()
            .zip(window.iter())
            .map(|(&x, &wi)| (x - mean) * wi)
            .collect();
        engine
            .process_with_scratch(&mut buf, &mut out, &mut scratch)
            .map_err(|e| DspError::InvalidConfig(format!("real FFT failed: {e}")))?;
        times[j] = (start + w / 2) as f64 / fs;
        for k in 1..=half {
            let scale = if w.is_multiple_of(2) && k == half { 1.0 } else { 2.0 } / wsum;
            magnitudes[[k - 1, j]] = out[k].norm() * scale;
        }
    }

    Ok(Spectrogram {
        times,
        freqs,
        magnitudes,
    })
}

impl CsiSignal {
    /// STFT of every amplitude subcarrier.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::{CsiSignal, StftConfig, Window};
    ///
    /// let fs = 20.0;
    /// let n = 400;
    /// let iq = Array2::from_shape_fn((n, 2), |(i, _)| {
    ///     let t = i as f64 / fs;
    ///     Complex::new((2.0 * std::f64::consts::PI * 0.3 * t).sin(), 0.0)
    /// });
    /// let ticks = Array1::from_iter((0..n).map(|i| (i as f64 / fs * 1e6) as u64));
    /// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
    /// let config = StftConfig::with_window(200, Window::Hann).unwrap();
    /// let specs = signal.stft_subcarriers(&config).unwrap();
    /// assert_eq!(specs.len(), 2);
    /// assert_eq!(specs[0].magnitudes.dim(), (specs[0].freqs.len(), specs[0].times.len()));
    /// ```
    pub fn stft_subcarriers(&self, config: &StftConfig) -> Result<Vec<Spectrogram>, DspError> {
        let mut specs = Vec::with_capacity(self.n_subcarriers());
        for k in 0..self.n_subcarriers() {
            let col = self.column(k);
            specs.push(stft(
                col.as_slice().expect("owned column is contiguous"),
                self.fs,
                config,
            )?);
        }
        Ok(specs)
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

    #[test]
    fn freq_bins_are_one_sided() {
        let bins = freq_bins(8, 100.0);
        assert_eq!(bins.len(), 5);
        assert!((bins[4] - 50.0).abs() < 1e-12);
        let bins_odd = freq_bins(7, 100.0);
        assert_eq!(bins_odd.len(), 4); // k = 0..=3
    }

    #[test]
    fn fft_finds_sine_peak_within_one_bin() {
        let fs = 100.0;
        let n = 1000;
        for freq in [0.5, 2.0, 7.3, 20.0] {
            let signal = sine(n, fs, freq, 1.0);
            let spec = fft(&signal, fs, Window::Hann).unwrap();
            let (best, _) = spec
                .magnitude
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            let peak_hz = spec.freqs[best];
            assert!(
                (peak_hz - freq).abs() <= fs / n as f64 + 1e-9,
                "freq {freq}: peak at {peak_hz}"
            );
        }
    }

    #[test]
    fn fft_hann_amplitude_scaling_is_correct() {
        let fs = 100.0;
        let n = 2000; // 5 Hz lands exactly on a bin (bin 100)
        let signal = sine(n, fs, 5.0, 3.0);
        let spec = fft(&signal, fs, Window::Hann).unwrap();
        let (best, &mag) = spec
            .magnitude
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert!((spec.freqs[best] - 5.0).abs() < 1e-12);
        assert!((mag - 3.0).abs() < 0.02, "magnitude {mag}");
        // Rectangular window too.
        let spec_none = fft(&signal, fs, Window::None).unwrap();
        let (_, mag_none) = spec_none
            .magnitude
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert!((mag_none - 3.0).abs() < 0.02);
    }

    #[test]
    fn fft_removes_dc() {
        let fs = 50.0;
        let n = 500;
        let signal: Vec<f64> = (0..n).map(|_| 42.0).collect(); // pure DC
        let spec = fft(&signal, fs, Window::Hann).unwrap();
        assert!(spec.magnitude.iter().all(|&m| m < 1e-9));
    }

    #[test]
    fn fft_rejects_bad_input() {
        assert!(matches!(fft(&[], 100.0, Window::Hann), Err(DspError::EmptySignal)));
        assert!(matches!(
            fft(&[1.0], 100.0, Window::Hann),
            Err(DspError::InvalidConfig(_))
        ));
        let s = sine(100, 100.0, 1.0, 1.0);
        assert!(matches!(
            fft(&s, 0.0, Window::Hann),
            Err(DspError::InvalidConfig(_))
        ));
    }

    #[test]
    fn dominant_peaks_respects_band_and_top_k() {
        let fs = 100.0;
        let n = 2000;
        let signal: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                2.0 * (2.0 * std::f64::consts::PI * 1.0 * t).sin()
                    + 1.0 * (2.0 * std::f64::consts::PI * 8.0 * t).sin()
            })
            .collect();
        let spec = fft(&signal, fs, Window::Hann).unwrap();
        let peaks = dominant_peaks(&spec, 0.5, 5.0, 5);
        // top_k bins sorted by magnitude: the strongest is the 1 Hz tone.
        assert_eq!(peaks.len(), 5);
        assert!((peaks[0].0 - 1.0).abs() <= fs / n as f64);
        assert!((peaks[0].1 - 2.0).abs() < 0.05);
        // Inverted band -> empty.
        assert!(dominant_peaks(&spec, 5.0, 0.5, 5).is_empty());
    }

    #[test]
    fn stft_constant_frequency_sine_has_flat_ridge() {
        let fs = 100.0;
        let n = 1000;
        let signal = sine(n, fs, 4.0, 1.0);
        let config = StftConfig::new(200, 100, Window::Hann).unwrap();
        let spec = stft(&signal, fs, &config).unwrap();
        assert_eq!(spec.times.len(), (n - 200) / 100 + 1);
        assert_eq!(spec.freqs.len(), 100);
        assert_eq!(spec.magnitudes.dim(), (100, spec.times.len()));
        // times are frame centers
        assert!((spec.times[0] - 1.0).abs() < 1e-12); // (0 + 100)/100
        for j in 0..spec.times.len() {
            let col = spec.magnitudes.column(j);
            let (best, &mag) = col
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            assert!((spec.freqs[best] - 4.0).abs() <= fs / 200.0 + 1e-9);
            assert!((mag - 1.0).abs() < 0.05);
        }
    }

    #[test]
    fn stft_chirp_ridge_tracks_instantaneous_frequency() {
        let fs = 200.0;
        let n = 4 * fs as usize; // 4 s
        let f0 = 2.0;
        let f1 = 18.0;
        let signal: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let f = f0 + (f1 - f0) * t / 4.0;
                (2.0 * std::f64::consts::PI * f * t).sin()
            })
            .collect();
        let config = StftConfig::new(200, 200, Window::Hann).unwrap(); // 1 s windows, no overlap
        let spec = stft(&signal, fs, &config).unwrap();
        let first_peak = spec
            .magnitudes
            .column(0)
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| spec.freqs[i])
            .unwrap();
        let last_peak = spec
            .magnitudes
            .column(spec.times.len() - 1)
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| spec.freqs[i])
            .unwrap();
        assert!(first_peak < 8.0, "first peak {first_peak}");
        assert!(last_peak > 14.0, "last peak {last_peak}");
        assert!(last_peak > first_peak);
    }

    #[test]
    fn stft_validates_config_and_length() {
        let signal = sine(100, 100.0, 1.0, 1.0);
        let config = StftConfig::new(200, 100, Window::Hann).unwrap();
        assert!(matches!(
            stft(&signal, 100.0, &config),
            Err(DspError::InvalidConfig(_))
        ));
        assert!(StftConfig::new(1, 1, Window::Hann).is_err());
        assert!(StftConfig::new(10, 0, Window::Hann).is_err());
        assert!(StftConfig::new(10, 11, Window::Hann).is_err());
        assert!(matches!(
            stft(&signal, -1.0, &StftConfig::default()),
            Err(DspError::InvalidConfig(_))
        ));
    }

    #[test]
    fn spectrogram_to_db_scales_correctly() {
        let fs = 50.0;
        let signal = sine(500, fs, 1.0, 1.0);
        let config = StftConfig::new(100, 50, Window::Hann).unwrap();
        let spec = stft(&signal, fs, &config).unwrap();
        let db = spec.to_db();
        let max_linear = spec.magnitudes.iter().copied().fold(0.0f64, f64::max);
        let max_db = db.magnitudes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!((max_db - 20.0 * max_linear.log10()).abs() < 1e-9);
    }

    #[test]
    fn window_sums_match_theory() {
        let n = 1000;
        assert!((Window::Hann.sum(n) - n as f64 / 2.0).abs() < 1e-6);
        assert!((Window::None.sum(n) - n as f64).abs() < 1e-9);
        // Hamming/Blackman sums are approximately 0.54n and 0.42n.
        assert!((Window::Hamming.sum(n) - 0.54 * n as f64).abs() < 1e-6);
        assert!((Window::Blackman.sum(n) - 0.42 * n as f64).abs() < 1e-6);
    }
}
