//! Integration tests for the `dsp` module, all on synthetic signals
//! (no captures, no fixtures).

use ndarray::{Array1, Array2};
use ndarray_npy::NpzWriter;
use rustfft::num_complex::Complex;

use rt_wireless_sensing::dsp::{
    CsiSignal, DspError, InterpMethod, NormScheme, StftConfig, Window, csi_ratio, dominant_peaks,
    fft, stft,
};
use rt_wireless_sensing::saving::TIMESTAMP_ARRAY;

/// Synthetic complex signal: `sc` subcarriers, each a sum of sines with a
/// phase rotation, sampled on a jittered ~`fs` clock.
fn synthetic_signal(n: usize, sc: usize, fs: f64) -> CsiSignal {
    let ticks = Array1::from_iter((0..n).map(|i| {
        let jitter = if i % 4 == 0 { 173.0 } else { -61.0 };
        ((i as f64 / fs) * 1e6 + jitter).round().max(0.0) as u64
    }));
    let iq = Array2::from_shape_fn((n, sc), |(i, k)| {
        let t = i as f64 / fs;
        let f = 0.3 + 0.1 * k as f64;
        // Positive amplitude: DC offset + tone, so the amplitude spectrum
        // keeps the tone at f Hz (a signed sine would rectify to 2f).
        let base = 4.0 + 0.5 * k as f64;
        let v = base + base * 0.4 * (2.0 * std::f64::consts::PI * f * t).sin();
        let rot = 0.05 * k as f64;
        Complex::new(v * (rot * t).cos(), v * (rot * t).sin())
    });
    CsiSignal::from_iq(iq, ticks).unwrap()
}

#[test]
fn full_chain_resample_normalize_hampel_bandpass_stft() {
    let fs_src = 20.0;
    let n = 800; // 40 s
    let signal = synthetic_signal(n, 4, fs_src);

    // resample onto a uniform 50 Hz grid
    let signal = signal.resample(50.0, InterpMethod::CubicSpline).unwrap();
    assert!((signal.fs - 50.0).abs() < 1e-12);
    // ~40 s at 50 Hz (tick jitter makes the exact count slightly smaller).
    assert!(signal.n_samples() >= (39.9 * 50.0) as usize);

    // normalize per subcarrier, remove outliers, isolate breathing band
    let signal = signal.normalize(NormScheme::PerSubcarrier);
    let signal = signal.hampel(7, 3.0).unwrap();
    let signal = signal.bandpass(0.1, 0.5).unwrap();

    // STFT: consistent shapes and axes
    let config = StftConfig::with_window(500, Window::Hann).unwrap(); // 10 s at 50 Hz
    let specs = signal.stft_subcarriers(&config).unwrap();
    assert_eq!(specs.len(), 4);
    for spec in &specs {
        assert_eq!(
            spec.magnitudes.dim(),
            (spec.freqs.len(), spec.times.len())
        );
        assert!(spec.times.len() >= 2);
        // frame centers are inside the record
        let dur = signal.duration_s();
        assert!(spec.times[0] > 0.0 && spec.times[spec.times.len() - 1] < dur);
        // freqs are the one-sided bins of the window
        assert!((spec.freqs[0] - 50.0 / 500.0).abs() < 1e-12);
    }

    // FFT per subcarrier finds the breathing tone of subcarrier 0 (0.3 Hz)
    let col = signal.column(0);
    let spectrum = fft(col.as_slice().unwrap(), signal.fs, Window::Hann).unwrap();
    let peaks = dominant_peaks(&spectrum, 0.1, 0.5, 3);
    assert!(!peaks.is_empty());
    assert!((peaks[0].0 - 0.3).abs() <= signal.fs / col.len() as f64 + 1e-9);
}

#[test]
fn csi_ratio_then_normalize_keeps_shapes() {
    let signal = synthetic_signal(300, 3, 20.0);
    let ratio = csi_ratio(&signal);
    assert_eq!(ratio.amplitude.dim(), signal.amplitude.dim());
    assert_eq!(ratio.ticks_us, signal.ticks_us);
    let z = ratio.normalize(NormScheme::PerSubcarrier);
    assert_eq!(z.amplitude.dim(), signal.amplitude.dim());
    for k in 0..3 {
        let col = z.amplitude.column(k);
        let mean = col.sum() / col.len() as f64;
        assert!(mean.abs() < 1e-9);
    }
}

#[test]
fn npz_roundtrip_matches_saving_layout() {
    let signal = synthetic_signal(120, 3, 20.0);

    // Write an archive in exactly the layout `saving` produces.
    let path = std::env::temp_dir().join(format!(
        "rt_ws_dsp_test_{}_{}.npz",
        std::process::id(),
        "roundtrip"
    ));
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut npz = NpzWriter::new(file);
        let ts: Array1<f64> = signal
            .ticks_us
            .mapv(|t| (t - signal.ticks_us[0]) as f64 / 1_000.0);
        npz.add_array(TIMESTAMP_ARRAY, &ts).unwrap();
        for k in 0..signal.n_subcarriers() {
            let amp: Array1<f32> = signal.amplitude.column(k).mapv(|v| v as f32);
            let pha: Array1<f32> = signal.phase.column(k).mapv(|v| v as f32);
            npz.add_array(format!("amplitude_{k:02}"), &amp).unwrap();
            npz.add_array(format!("phase_{k:02}"), &pha).unwrap();
        }
        npz.finish().unwrap();
    }

    let loaded = CsiSignal::from_npz(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(loaded.n_samples(), signal.n_samples());
    assert_eq!(loaded.n_subcarriers(), signal.n_subcarriers());
    assert!((loaded.fs - signal.fs).abs() / signal.fs < 1e-3);
    // amplitude round-trips through f32 within tolerance
    let max_err = loaded
        .amplitude
        .iter()
        .zip(signal.amplitude.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(max_err < 1e-4, "max amplitude error {max_err}");
    // complex view is reconstructed from amplitude + phase
    let cx = loaded.complex();
    assert!((cx[[0, 0]].norm() - loaded.amplitude[[0, 0]]).abs() < 1e-12);
}

#[test]
fn from_npz_reports_missing_arrays() {
    let path = std::env::temp_dir().join(format!(
        "rt_ws_dsp_test_{}_{}.npz",
        std::process::id(),
        "missing"
    ));
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut npz = NpzWriter::new(file);
        npz.add_array(TIMESTAMP_ARRAY, &Array1::from_vec(vec![0.0f64, 50.0]))
            .unwrap();
        npz.finish().unwrap();
    }
    let result = CsiSignal::from_npz(&path);
    std::fs::remove_file(&path).ok();
    assert!(matches!(result, Err(DspError::MissingArray(_))));
}

#[test]
fn stft_axes_match_config() {
    let signal = synthetic_signal(600, 1, 20.0);
    let config = StftConfig::new(100, 25, Window::Blackman).unwrap();
    let spec = stft(
        signal.column(0).as_slice().unwrap(),
        signal.fs,
        &config,
    )
    .unwrap();
    // frames: starts 0,25,...,500 -> 21 frames; bins 1..=50 -> 50 rows
    assert_eq!(spec.times.len(), (600 - 100) / 25 + 1);
    assert_eq!(spec.freqs.len(), 50);
    assert_eq!(spec.magnitudes.dim(), (50, 21));
    // time axis: center of frame j = (25j + 50) / 20 s
    assert!((spec.times[1] - (25.0 + 50.0) / 20.0).abs() < 1e-12);
    // dB conversion is monotone-preserving on the peak
    let db = spec.to_db();
    assert_eq!(db.magnitudes.dim(), spec.magnitudes.dim());
}
