//! Synchronous signal processing for decoded CSI records.
//!
//! This module is the DSP stage of the pipeline. It operates **only on decoded
//! CSI arrays** — never on raw `.bin` bytes — so there is exactly one decode
//! path in the crate ([`crate::decoder`] via [`crate::saving`]). All functions
//! are synchronous, deterministic transforms over [`ndarray`] arrays: no
//! tokio, no network, and no file I/O inside the DSP functions themselves. The
//! only I/O is in the two thin loaders ([`CsiSignal::from_capture`] and
//! [`CsiSignal::from_npz`]), which wrap the existing decode/save output.
//!
//! # Pipeline overview
//!
//! ```text
//! capture_v8.bin ──decoder──▶ CsiPacket[] ──▶ CsiSignal::from_capture
//! capture_v8.npz  ──saving───┘                  │
//!                                               ▼
//!                              normalize ──▶ resample ──▶ outlier removal
//!                                               │            (Hampel / median)
//!                                               ▼
//!                                   biquad filters (LPF/HPF/BPF)
//!                                               │
//!                                     ┌─────────┴─────────┐
//!                                     ▼                   ▼
//!                              fft / dominant_peaks    stft → Spectrogram
//! ```
//!
//! # The core type
//!
//! [`CsiSignal`] bundles everything a DSP operation needs:
//!
//! ```text
//! amplitude  (n_samples × n_subcarriers)  |h_k(t)| = √( I_k(t)² + Q_k(t)² )
//! phase      (n_samples × n_subcarriers)  φ_k(t)   = atan2(Q_k(t), I_k(t))   raw, unsanitized
//! ticks_us   (n_samples)                  device_ticks_us, non-decreasing
//! fs         nominal sample rate in Hz    1e6 / median(tick interval)
//! iq         optional complex baseband    h_k(t) = I_k(t) + j·Q_k(t)
//! ```
//!
//! In LaTeX:
//!
//! $$|h_k(t)| = \sqrt{I_k(t)^2 + Q_k(t)^2}, \qquad
//!    \varphi_k(t) = \operatorname{atan2}(Q_k(t), I_k(t))$$
//!
//! Amplitude is the primary signal; phase is stored raw and unsanitized (wrapped
//! to $[-\pi, \pi]$, never unwrapped). The complex I/Q values are kept (or
//! reconstructed from amplitude and phase) because resampling must interpolate
//! I and Q separately — never amplitude, and never raw phase, which has
//! wrap-around discontinuities.
//!
//! # Usage
//!
//! ```no_run
//! use std::path::Path;
//! use rt_wireless_sensing::dsp::{
//!     CsiSignal, DspError, InterpMethod, NormScheme, Window,
//!     dominant_peaks, fft,
//! };
//!
//! # fn example() -> Result<(), DspError> {
//! // 1. Load a capture (thin wrapper over decoder/saving output).
//! let signal = CsiSignal::from_capture(Path::new("artifacts/capture_v8.bin"))?;
//!
//! // 2. Clean and standardize: normalize, resample to a uniform grid,
//! //    remove impulse outliers.
//! let signal = signal.normalize(NormScheme::PerSubcarrier);
//! let signal = signal.resample(20.0, InterpMethod::CubicSpline)?;
//! let signal = signal.hampel(5, 3.0)?;
//!
//! // 3. Isolate a band of interest and look at the spectrum per subcarrier.
//! let signal = signal.bandpass(0.1, 0.5)?; // breathing band, Hz
//! for sc in 0..signal.n_subcarriers() {
//!     let col = signal.column(sc);
//!     let spectrum = fft(col.as_slice().unwrap(), signal.fs, Window::Hann)?;
//!     let peaks = dominant_peaks(&spectrum, 0.1, 0.5, 3);
//!     println!("subcarrier {sc}: {peaks:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # How to choose
//!
//! | Goal | Suggested chain |
//! |---|---|
//! | Breathing / vital signs (0.1–0.5 Hz) | `bandpass(0.1, 0.5)` → `fft` + `dominant_peaks` |
//! | Motion / gestures (0.2–10 Hz) | `hampel` → `highpass(0.2)` → `stft` |
//! | General cleanup | `hampel` → `normalize(PerSubcarrier)` |
//! | Comparing records with different packet rates | `resample(fs, CubicSpline)` first |
//!
//! # Dependency decisions
//!
//! Per the project dependency policy (use a maintained crate when it adds the
//! function directly without dragging in async/networking machinery):
//!
//! | Decision | Choice | Why |
//! |---|---|---|
//! | Real-input FFT | `realfft` crate | one-sided spectrum directly, ~2× faster than a complex FFT; shares `rustfft` 6.4 |
//! | Biquad filters | `biquad` crate (RBJ cookbook, `no_std`) | `DirectForm2Transposed` runs sample-by-sample, which fits per-subcarrier column processing; band-pass is a HPF+LPF cascade |
//! | Cubic-spline resampling | hand-rolled natural cubic spline ([`resample`]) | no maintained crate evaluates a *non-uniform* natural cubic spline (`splines`' Catmull-Rom requires uniform spacing, which jittered device ticks violate); the Thomas-algorithm solve is ~40 lines and unit-tested |
//! | Medians (Hampel/median filter) | hand-rolled quickselect (`select_nth_unstable`) | `ndarray-stats` 0.7's quantile API is in-place on `ArrayRef` and does not fit read-only sliding windows over `&[f64]`; window sizes are small |
//!
//! DWT/wavelets and PCA/compression are explicitly out of scope for this module.
//! Integration with [`crate::visualization`] is future work; [`spectral::Spectrum`]
//! and [`spectral::Spectrogram`] are plain-data structs so a renderer can consume
//! them without depending on dsp internals.

use std::{error::Error, fmt, fs::File, io, path::Path};

use ndarray::{Array1, Array2};
use ndarray_npy::{NpzReader, ReadNpzError};
use rustfft::num_complex::Complex;

use crate::decoder::{CsiPacket, DecodeError};
use crate::saving::{TIMESTAMP_ARRAY, decode_capture};

pub mod filter;
pub mod preprocess;
pub mod resample;
pub mod spectral;

pub use filter::{
    bandpass, exp_moving_average, hampel_filter, highpass, lowpass, median_filter, moving_average,
};
pub use preprocess::{NormScheme, csi_ratio};
pub use resample::{GridState, InterpMethod, ResampleOptions, ResampleOutput};
pub use spectral::{Spectrogram, Spectrum, StftConfig, Window, dominant_peaks, fft, freq_bins, stft};

/// Epsilon guard against near-zero variance in normalization and min-max.
pub(crate) const EPS: f64 = 1e-12;

/// Count of [`NULL_BINS_HT20`], kept as a standalone constant for array sizing.
const NULL_BINS_HT20_COUNT: usize = 8;

/// Null (guard/pilot-nulling) subcarrier indices of a centred HT20 capture:
/// the four lowest and four highest bins plus the DC bin, for 64 subcarriers.
///
/// These subcarriers carry no useful channel information — the ESP32 reports
/// them with zero (or near-zero) amplitude — so they must never be rendered as
/// real `0` values and must be excluded from any transform that works across
/// the subcarrier axis.
pub const NULL_BINS_HT20: [usize; NULL_BINS_HT20_COUNT] = [0, 1, 2, 3, 32, 61, 62, 63];

/// Number of subcarriers of the HT20 layout [`NULL_BINS_HT20`] describes.
pub const HT20_SUBCARRIERS: usize = 64;

/// Null bins of the HT20 layout, or an empty list for any other subcarrier
/// count (a non-HT20 capture has no known null-bin table here).
pub fn null_bins_for(subcarriers: usize) -> Vec<usize> {
    if subcarriers == HT20_SUBCARRIERS {
        NULL_BINS_HT20.to_vec()
    } else {
        Vec::new()
    }
}

/// A decoded CSI record set ready for signal processing.
///
/// Shapes: `amplitude` and `phase` are `(n_samples, n_subcarriers)`,
/// `ticks_us` has length `n_samples`, and `iq` (when present) matches
/// `amplitude`. Construct with [`CsiSignal::new`], [`CsiSignal::from_packets`],
/// [`CsiSignal::from_capture`], [`CsiSignal::from_npz`], or
/// [`CsiSignal::from_iq`]; every DSP operation takes and returns this type so
/// the pipeline chains naturally.
#[derive(Debug, Clone)]
pub struct CsiSignal {
    /// Amplitude $|h_k(t)|$, shape `(n_samples, n_subcarriers)`. Primary signal.
    pub amplitude: Array2<f64>,
    /// Raw (unsanitized, wrapped) phase in radians, same shape as `amplitude`.
    pub phase: Array2<f64>,
    /// Per-sample device timestamps in microseconds (`device_ticks_us`),
    /// length `n_samples`, non-decreasing.
    pub ticks_us: Array1<u64>,
    /// Nominal sample rate in Hz, derived from the median tick interval.
    pub fs: f64,
    /// Complex baseband $h_k(t) = I_k(t) + jQ_k(t)$, same shape as
    /// `amplitude`. Filled by [`CsiSignal::from_capture`] /
    /// [`CsiSignal::from_packets`] / [`CsiSignal::from_iq`]; `None` when the
    /// signal was built from amplitude/phase only (then
    /// [`CsiSignal::complex`] reconstructs it).
    pub iq: Option<Array2<Complex<f64>>>,
    /// Indices of the null (guard/DC) subcarriers, ascending. Empty by default;
    /// [`CsiSignal::from_packets`] fills [`NULL_BINS_HT20`] for 64-subcarrier
    /// captures.
    ///
    /// Null subcarriers carry no channel information: they are never rendered
    /// as a real value and are excluded from every transform that works across
    /// the subcarrier axis (see [`CsiSignal::normalize`]).
    pub null_bins: Vec<usize>,
}

/// Failure modes of the dsp module.
///
/// Mirrors the style of [`crate::collection::CollectionError`] and
/// [`crate::saving::SaveError`]: a concrete enum, no `Box<dyn Error>` in public
/// signatures.
#[derive(Debug)]
pub enum DspError {
    /// Reading a capture or archive file failed.
    Io(io::Error),
    /// The `.bin` capture is malformed or truncated.
    Decode(DecodeError),
    /// Reading an `.npz` archive failed.
    Npz(ReadNpzError),
    /// A required array is missing from an `.npz` archive.
    MissingArray(String),
    /// The signal has zero samples.
    EmptySignal,
    /// Arrays have inconsistent shapes (details in the message).
    InvalidShape(String),
    /// `ticks_us` moved backwards.
    NonMonotonicTicks {
        /// Index of the offending sample.
        index: usize,
        /// Tick value of the previous sample.
        previous: u64,
        /// Tick value found at `index`.
        found: u64,
    },
    /// A statistic was requested over data with zero variance (e.g. all tick
    /// intervals are zero, so no sample rate exists).
    ZeroVariance(String),
    /// A parameter is out of range (bad sample rate, window size, cutoff…).
    InvalidConfig(String),
}

impl fmt::Display for DspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error in dsp: {error}"),
            Self::Decode(error) => write!(f, "cannot decode capture: {error}"),
            Self::Npz(error) => write!(f, "cannot read .npz archive: {error}"),
            Self::MissingArray(name) => write!(f, "archive is missing array {name:?}"),
            Self::EmptySignal => write!(f, "signal has zero samples"),
            Self::InvalidShape(msg) => write!(f, "invalid array shape: {msg}"),
            Self::NonMonotonicTicks {
                index,
                previous,
                found,
            } => write!(
                f,
                "ticks_us moved backwards at sample {index}: {previous} -> {found}"
            ),
            Self::ZeroVariance(msg) => write!(f, "zero variance: {msg}"),
            Self::InvalidConfig(msg) => write!(f, "invalid DSP configuration: {msg}"),
        }
    }
}

impl Error for DspError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Npz(error) => Some(error),
            Self::MissingArray(_)
            | Self::EmptySignal
            | Self::InvalidShape(_)
            | Self::NonMonotonicTicks { .. }
            | Self::ZeroVariance(_)
            | Self::InvalidConfig(_) => None,
        }
    }
}

impl From<io::Error> for DspError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DecodeError> for DspError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<ReadNpzError> for DspError {
    fn from(error: ReadNpzError) -> Self {
        Self::Npz(error)
    }
}

impl CsiSignal {
    /// Build a signal from amplitude, phase, and tick arrays.
    ///
    /// `amplitude` and `phase` must have the same `(n_samples, n_subcarriers)`
    /// shape with at least one sample and one subcarrier; `ticks_us` must have
    /// length `n_samples` and be non-decreasing. `fs` is derived from the
    /// median positive tick interval:
    ///
    /// $$f_s = \frac{10^6}{\operatorname{median}(\Delta\,ticks\_us)}$$
    ///
    /// `iq` is left as `None`; use [`CsiSignal::with_iq`] or
    /// [`CsiSignal::from_iq`] to attach complex data.
    ///
    /// # Errors
    ///
    /// [`DspError::EmptySignal`] when there are no samples,
    /// [`DspError::InvalidShape`] on shape mismatch, [`DspError::NonMonotonicTicks`]
    /// when ticks decrease, [`DspError::ZeroVariance`] when every tick
    /// interval is zero (no sample rate exists).
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let amplitude = Array2::from_shape_fn((4, 2), |(i, k)| (i + k) as f64);
    /// let phase = Array2::zeros((4, 2));
    /// let ticks = Array1::from_vec(vec![0, 50_000, 100_000, 150_000]);
    /// let signal = CsiSignal::new(amplitude, phase, ticks).unwrap();
    /// assert_eq!(signal.n_samples(), 4);
    /// assert_eq!(signal.n_subcarriers(), 2);
    /// assert!((signal.fs - 20.0).abs() < 1e-9);
    /// ```
    pub fn new(
        amplitude: Array2<f64>,
        phase: Array2<f64>,
        ticks_us: Array1<u64>,
    ) -> Result<Self, DspError> {
        let (n, sc) = amplitude.dim();
        if n == 0 || sc == 0 {
            return Err(DspError::EmptySignal);
        }
        if phase.dim() != (n, sc) {
            return Err(DspError::InvalidShape(format!(
                "phase shape {:?} does not match amplitude shape ({n}, {sc})",
                phase.dim()
            )));
        }
        if ticks_us.len() != n {
            return Err(DspError::InvalidShape(format!(
                "ticks_us has length {}, expected {n}",
                ticks_us.len()
            )));
        }
        validate_ticks(&ticks_us)?;
        let fs = sample_rate_from_ticks(&ticks_us)?;
        Ok(Self {
            amplitude,
            phase,
            ticks_us,
            fs,
            iq: None,
            null_bins: Vec::new(),
        })
    }

    /// Attach complex I/Q data, checking it matches the amplitude shape.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let amplitude = Array2::from_elem((3, 1), 2.0);
    /// let phase = Array2::zeros((3, 1));
    /// let ticks = Array1::from_vec(vec![0, 10_000, 20_000]);
    /// let iq = Array2::from_elem((3, 1), Complex::new(2.0, 0.0));
    /// let signal = CsiSignal::new(amplitude, phase, ticks).unwrap().with_iq(iq).unwrap();
    /// assert!(signal.iq.is_some());
    /// ```
    pub fn with_iq(mut self, iq: Array2<Complex<f64>>) -> Result<Self, DspError> {
        if iq.dim() != self.amplitude.dim() {
            return Err(DspError::InvalidShape(format!(
                "iq shape {:?} does not match amplitude shape {:?}",
                iq.dim(),
                self.amplitude.dim()
            )));
        }
        self.iq = Some(iq);
        Ok(self)
    }

    /// Build a signal from complex I/Q values and ticks, deriving amplitude
    /// and phase:
    ///
    /// $$|h_k(t)| = |h_k(t)|, \qquad \varphi_k(t) = \arg h_k(t)$$
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rustfft::num_complex::Complex;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let iq = Array2::from_shape_fn((3, 1), |(i, _)| Complex::new(3.0, 4.0 + i as f64));
    /// let ticks = Array1::from_vec(vec![0, 50_000, 100_000]);
    /// let signal = CsiSignal::from_iq(iq, ticks).unwrap();
    /// assert!((signal.amplitude[[0, 0]] - 5.0).abs() < 1e-12);
    /// ```
    pub fn from_iq(iq: Array2<Complex<f64>>, ticks_us: Array1<u64>) -> Result<Self, DspError> {
        let amplitude = iq.mapv(|c| c.norm());
        let phase = iq.mapv(|c| c.arg());
        let mut signal = Self::new(amplitude, phase, ticks_us)?;
        signal.iq = Some(iq);
        Ok(signal)
    }

    /// Build a signal from decoded packets (the output of
    /// [`crate::decoder::FrameStreamDecoder`]).
    ///
    /// Amplitude, phase, and complex I/Q come from each packet's
    /// [`CsiPacket::iq_pairs`] (`I = iq_raw[2k+1]`, `Q = iq_raw[2k]`, the
    /// ESPectre wire order); ticks come from `csi.device_ticks_us`.
    ///
    /// # Errors
    ///
    /// [`DspError::EmptySignal`] for an empty packet list,
    /// [`DspError::InvalidShape`] when subcarrier counts differ between
    /// packets, [`DspError::NonMonotonicTicks`] on decreasing ticks.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use rt_wireless_sensing::decoder::FrameStreamDecoder;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let bytes = std::fs::read("artifacts/capture_v8.bin").unwrap();
    /// let mut decoder = FrameStreamDecoder::with_capacity(bytes.len());
    /// decoder.push(&bytes);
    /// let mut packets = Vec::new();
    /// while let Some(packet) = decoder.next_frame().unwrap() {
    ///     packets.push(packet);
    /// }
    /// let signal = CsiSignal::from_packets(&packets).unwrap();
    /// println!("{} samples x {} subcarriers @ {:.2} Hz",
    ///     signal.n_samples(), signal.n_subcarriers(), signal.fs);
    /// ```
    pub fn from_packets(packets: &[CsiPacket]) -> Result<Self, DspError> {
        let first = packets.first().ok_or(DspError::EmptySignal)?;
        let subcarriers = first.subcarriers();
        if subcarriers == 0 {
            return Err(DspError::InvalidShape(
                "first record reports zero subcarriers".to_string(),
            ));
        }
        let n = packets.len();
        let mut amplitude = Array2::<f64>::zeros((n, subcarriers));
        let mut phase = Array2::<f64>::zeros((n, subcarriers));
        let mut iq = Array2::<Complex<f64>>::zeros((n, subcarriers));
        let mut ticks_us = Array1::<u64>::zeros(n);

        for (i, packet) in packets.iter().enumerate() {
            if packet.subcarriers() != subcarriers {
                return Err(DspError::InvalidShape(format!(
                    "record {i} has {} subcarriers, expected {subcarriers}",
                    packet.subcarriers()
                )));
            }
            ticks_us[i] = packet.csi.device_ticks_us;
            for (k, (re, im)) in packet.iq_pairs().enumerate() {
                let re = re as f64;
                let im = im as f64;
                iq[[i, k]] = Complex::new(re, im);
                amplitude[[i, k]] = re.hypot(im);
                phase[[i, k]] = im.atan2(re);
            }
        }

        let mut signal = Self::new(amplitude, phase, ticks_us)?;
        signal.iq = Some(iq);
        // A 64-subcarrier capture is HT20: mark its guard/DC bins so that
        // normalization and every cross-subcarrier transform skip them.
        signal.null_bins = null_bins_for(subcarriers);
        Ok(signal)
    }

    /// Decode a raw `.bin` capture and build a signal from it.
    ///
    /// This is a thin convenience loader: the bytes are decoded with the same
    /// [`crate::decoder::FrameStreamDecoder`] path used by
    /// [`crate::saving`] (there is no second decode path), and the packets are
    /// converted with [`CsiSignal::from_packets`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::path::Path;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let signal = CsiSignal::from_capture(Path::new("artifacts/capture_v8.bin")).unwrap();
    /// println!("{} x {} @ {:.2} Hz", signal.n_samples(), signal.n_subcarriers(), signal.fs);
    /// ```
    pub fn from_capture(path: &Path) -> Result<Self, DspError> {
        let bytes = std::fs::read(path)?;
        let packets = decode_capture(&bytes).map_err(|e| match e {
            crate::saving::SaveError::Decode(e) => DspError::Decode(e),
            crate::saving::SaveError::Io(e) => DspError::Io(e),
            other => DspError::InvalidShape(format!("capture rejected by saving layer: {other}")),
        })?;
        Self::from_packets(&packets)
    }

    /// Load a signal from a `.npz` archive written by [`crate::saving`].
    ///
    /// Expected layout (see [`crate::saving`]): a `timestamp_ms` `(N,)` `f64`
    /// array plus `amplitude_00…` / `phase_00…` `(N,)` `f32` arrays, one per
    /// subcarrier. Ticks are reconstructed as `timestamp_ms * 1000` rounded to
    /// `u64`, and the complex I/Q values are reconstructed exactly from
    /// amplitude and phase, so resampling and [`csi_ratio`] work on `.npz`
    /// input too.
    ///
    /// # Errors
    ///
    /// [`DspError::Io`] / [`DspError::Npz`] on file problems,
    /// [`DspError::MissingArray`] when `timestamp_ms` or any amplitude/phase
    /// array is absent, [`DspError::InvalidShape`] when the per-subcarrier
    /// arrays are inconsistent.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::path::Path;
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let signal = CsiSignal::from_npz(Path::new("artifacts/capture_v8.npz")).unwrap();
    /// println!("{} x {} @ {:.2} Hz", signal.n_samples(), signal.n_subcarriers(), signal.fs);
    /// ```
    pub fn from_npz(path: &Path) -> Result<Self, DspError> {
        let file = File::open(path)?;
        let mut npz = NpzReader::new(file)?;

        let timestamp_ms: Array1<f64> = npz
            .by_name(TIMESTAMP_ARRAY)
            .map_err(|_| DspError::MissingArray(TIMESTAMP_ARRAY.to_string()))?;
        let n = timestamp_ms.len();
        if n == 0 {
            return Err(DspError::EmptySignal);
        }

        let names = npz.names()?;
        let mut amplitude_names: Vec<(usize, &str)> = names
            .iter()
            .filter_map(|name| {
                name.strip_prefix("amplitude_")
                    .and_then(|idx| idx.parse::<usize>().ok())
                    .map(|idx| (idx, name.as_str()))
            })
            .collect();
        amplitude_names.sort_by_key(|(idx, _)| *idx);
        if amplitude_names.is_empty() {
            return Err(DspError::MissingArray("amplitude_*".to_string()));
        }
        let subcarriers = amplitude_names.len();
        for (expected, (idx, _)) in amplitude_names.iter().enumerate() {
            if *idx != expected {
                return Err(DspError::InvalidShape(format!(
                    "amplitude array indices are not contiguous: expected {expected}, found {idx}"
                )));
            }
        }

        let mut amplitude = Array2::<f64>::zeros((n, subcarriers));
        let mut phase = Array2::<f64>::zeros((n, subcarriers));
        for (k, (_, amp_name)) in amplitude_names.iter().enumerate() {
            let column: Array1<f32> = npz.by_name(amp_name)?;
            if column.len() != n {
                return Err(DspError::InvalidShape(format!(
                    "array {amp_name:?} has length {}, expected {n}",
                    column.len()
                )));
            }
            amplitude.column_mut(k).assign(&column.mapv(f64::from));

            let phase_name = format!("phase_{k:02}");
            let phase_column: Array1<f32> = npz
                .by_name(&phase_name)
                .map_err(|_| DspError::MissingArray(phase_name.clone()))?;
            if phase_column.len() != n {
                return Err(DspError::InvalidShape(format!(
                    "array {phase_name:?} has length {}, expected {n}",
                    phase_column.len()
                )));
            }
            phase.column_mut(k).assign(&phase_column.mapv(f64::from));
        }

        let ticks_us = timestamp_ms.mapv(|ms| (ms * 1_000.0).round().max(0.0) as u64);
        Self::new(amplitude, phase, ticks_us)
    }

    /// Number of samples (rows).
    pub fn n_samples(&self) -> usize {
        self.amplitude.nrows()
    }

    /// Number of subcarriers (columns).
    pub fn n_subcarriers(&self) -> usize {
        self.amplitude.ncols()
    }

    /// Duration of the record in seconds, from tick span.
    pub fn duration_s(&self) -> f64 {
        let n = self.ticks_us.len();
        if n < 2 {
            0.0
        } else {
            (self.ticks_us[n - 1] - self.ticks_us[0]) as f64 / 1e6
        }
    }

    /// Sample times in seconds relative to the first tick:
    /// $t_i = (ticks\_us_i - ticks\_us_0) / 10^6$.
    pub fn times_s(&self) -> Array1<f64> {
        let t0 = self.ticks_us[0];
        self.ticks_us.mapv(|t| (t - t0) as f64 / 1e6)
    }

    /// Amplitude of one subcarrier as an owned vector, shape `(n_samples,)`.
    ///
    /// # Example
    ///
    /// ```
    /// use ndarray::{Array1, Array2};
    /// use rt_wireless_sensing::dsp::CsiSignal;
    ///
    /// let amplitude = Array2::from_shape_fn((3, 2), |(i, k)| (10 * i + k) as f64);
    /// let phase = Array2::zeros((3, 2));
    /// let ticks = Array1::from_vec(vec![0, 50_000, 100_000]);
    /// let signal = CsiSignal::new(amplitude, phase, ticks).unwrap();
    /// assert_eq!(signal.column(1).as_slice().unwrap(), &[1.0, 11.0, 21.0]);
    /// ```
    pub fn column(&self, subcarrier: usize) -> Array1<f64> {
        self.amplitude.column(subcarrier).to_owned()
    }

    /// Complex baseband $h_k(t)$, either the stored `iq` or reconstructed from
    /// amplitude and phase as $h = |h| e^{j\varphi}$.
    pub fn complex(&self) -> Array2<Complex<f64>> {
        match &self.iq {
            Some(iq) => iq.clone(),
            None => Array2::from_shape_fn(self.amplitude.dim(), |(i, k)| {
                let a = self.amplitude[[i, k]];
                let p = self.phase[[i, k]];
                Complex::new(a * p.cos(), a * p.sin())
            }),
        }
    }

    /// Return a copy with `amplitude` replaced; `iq` is recomputed from the new
    /// amplitude and the existing phase so the signal stays self-consistent.
    pub(crate) fn with_amplitude(&self, amplitude: Array2<f64>) -> Self {
        let iq = Array2::from_shape_fn(amplitude.dim(), |(i, k)| {
            let a = amplitude[[i, k]];
            let p = self.phase[[i, k]];
            Complex::new(a * p.cos(), a * p.sin())
        });
        Self {
            amplitude,
            phase: self.phase.clone(),
            ticks_us: self.ticks_us.clone(),
            fs: self.fs,
            iq: Some(iq),
            null_bins: self.null_bins.clone(),
        }
    }

    /// Apply a 1-D filter to every amplitude subcarrier column.
    pub(crate) fn map_amplitude_columns(
        &self,
        f: impl Fn(&[f64]) -> Result<Vec<f64>, DspError>,
    ) -> Result<Self, DspError> {
        let (n, sc) = self.amplitude.dim();
        let mut out = Array2::<f64>::zeros((n, sc));
        for k in 0..sc {
            let column = self.amplitude.column(k).to_vec();
            let filtered = f(&column)?;
            if filtered.len() != n {
                return Err(DspError::InvalidShape(format!(
                    "filter returned {} samples for subcarrier {k}, expected {n}",
                    filtered.len()
                )));
            }
            out.column_mut(k).assign(&Array1::from(filtered));
        }
        Ok(self.with_amplitude(out))
    }
}

/// Check that ticks are non-decreasing.
pub(crate) fn validate_ticks(ticks: &Array1<u64>) -> Result<(), DspError> {
    for (i, w) in ticks.windows(2).into_iter().enumerate() {
        if w[1] < w[0] {
            return Err(DspError::NonMonotonicTicks {
                index: i + 1,
                previous: w[0],
                found: w[1],
            });
        }
    }
    Ok(())
}

/// Nominal sample rate from the median positive tick interval:
/// $f_s = 10^6 / \operatorname{median}(\Delta ticks\_us)$.
pub(crate) fn sample_rate_from_ticks(ticks: &Array1<u64>) -> Result<f64, DspError> {
    if ticks.len() < 2 {
        return Err(DspError::EmptySignal);
    }
    let mut diffs: Vec<f64> = ticks
        .windows(2)
        .into_iter()
        .map(|w| (w[1] - w[0]) as f64)
        .filter(|&d| d > 0.0)
        .collect();
    if diffs.is_empty() {
        return Err(DspError::ZeroVariance(
            "all device tick intervals are zero; no sample rate exists".to_string(),
        ));
    }
    diffs.sort_by(f64::total_cmp);
    let median = diffs[diffs.len() / 2];
    Ok(1e6 / median)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(n: usize, sc: usize, dt_us: u64) -> CsiSignal {
        let amplitude = Array2::from_shape_fn((n, sc), |(i, k)| (i as f64 + 0.1 * k as f64).sin());
        let phase = Array2::from_shape_fn((n, sc), |(i, k)| (i as f64 * 0.01 + k as f64).cos());
        let ticks = Array1::from_iter((0..n).map(|i| (i as u64) * dt_us));
        CsiSignal::new(amplitude, phase, ticks).unwrap()
    }

    #[test]
    fn new_validates_shapes_and_computes_fs() {
        let signal = synthetic(10, 3, 50_000);
        assert_eq!(signal.n_samples(), 10);
        assert_eq!(signal.n_subcarriers(), 3);
        assert!((signal.fs - 20.0).abs() < 1e-9);
        assert!((signal.duration_s() - 0.45).abs() < 1e-12);
    }

    #[test]
    fn new_rejects_empty_and_mismatched_shapes() {
        let ticks = Array1::from_vec(vec![0u64, 1]);
        assert!(matches!(
            CsiSignal::new(Array2::zeros((0, 2)), Array2::zeros((0, 2)), Array1::zeros(0)),
            Err(DspError::EmptySignal)
        ));
        assert!(matches!(
            CsiSignal::new(Array2::zeros((2, 2)), Array2::zeros((2, 3)), ticks.clone()),
            Err(DspError::InvalidShape(_))
        ));
        assert!(matches!(
            CsiSignal::new(Array2::zeros((2, 2)), Array2::zeros((2, 2)), Array1::from_vec(vec![0u64])),
            Err(DspError::InvalidShape(_))
        ));
    }

    #[test]
    fn new_rejects_non_monotonic_ticks() {
        let ticks = Array1::from_vec(vec![0u64, 50_000, 40_000]);
        let result = CsiSignal::new(Array2::zeros((3, 1)), Array2::zeros((3, 1)), ticks);
        assert!(matches!(
            result,
            Err(DspError::NonMonotonicTicks {
                index: 2,
                previous: 50_000,
                found: 40_000
            })
        ));
    }

    #[test]
    fn zero_tick_variance_is_rejected() {
        let ticks = Array1::from_vec(vec![7u64, 7, 7]);
        let result = CsiSignal::new(Array2::zeros((3, 1)), Array2::zeros((3, 1)), ticks);
        assert!(matches!(result, Err(DspError::ZeroVariance(_))));
    }

    #[test]
    fn median_interval_ignores_jitter_and_duplicates() {
        // intervals: 40k, 60k, 50k, 50k, 0 (duplicate) -> positive median 50k
        let ticks = Array1::from_vec(vec![0u64, 40_000, 100_000, 150_000, 200_000, 200_000]);
        let fs = sample_rate_from_ticks(&ticks).unwrap();
        assert!((fs - 20.0).abs() < 1e-9);
    }

    #[test]
    fn from_iq_derives_amplitude_and_phase() {
        let iq = Array2::from_shape_fn((2, 1), |(i, _)| {
            Complex::new(1.0, if i == 0 { 0.0 } else { 1.0 })
        });
        let ticks = Array1::from_vec(vec![0u64, 50_000]);
        let signal = CsiSignal::from_iq(iq, ticks).unwrap();
        assert!((signal.amplitude[[0, 0]] - 1.0).abs() < 1e-12);
        assert!((signal.amplitude[[1, 0]] - 2f64.sqrt()).abs() < 1e-12);
        assert!((signal.phase[[1, 0]] - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
        assert!(signal.iq.is_some());
    }

    #[test]
    fn complex_reconstructs_from_amplitude_phase_when_iq_absent() {
        // Positive amplitudes so norm/arg round-trip without wrapping.
        let amplitude = Array2::from_shape_fn((5, 2), |(i, k)| 1.0 + i as f64 + 0.1 * k as f64);
        let phase = Array2::from_shape_fn((5, 2), |(i, k)| 0.3 * i as f64 - 0.2 * k as f64);
        let ticks = Array1::from_iter((0..5).map(|i| i as u64 * 50_000));
        let signal = CsiSignal::new(amplitude, phase, ticks).unwrap();
        assert!(signal.iq.is_none());
        let cx = signal.complex();
        for i in 0..5 {
            for k in 0..2 {
                let a = signal.amplitude[[i, k]];
                let p = signal.phase[[i, k]];
                assert!((cx[[i, k]].norm() - a).abs() < 1e-12);
                assert!((cx[[i, k]].arg() - p).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn with_amplitude_keeps_signal_consistent() {
        let signal = synthetic(6, 2, 50_000);
        let new_amp = Array2::from_elem((6, 2), 3.0);
        let out = signal.with_amplitude(new_amp);
        let cx = out.complex();
        assert!((cx[[0, 0]].norm() - 3.0).abs() < 1e-12);
        assert_eq!(out.ticks_us, signal.ticks_us);
        assert_eq!(out.fs, signal.fs);
    }
}
