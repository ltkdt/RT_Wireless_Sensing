# PLAN.md — DSP Module Rewrite (`src/dsp.rs`)

## Goal

Rewrite the deprecated `src/dsp.rs` into a clean, synchronous, file-I/O-free signal
processing module for CSI data produced by the existing `decoder`/`saving` pipeline
(ESPectre ESP32, `capture_v8.bin`). The module operates on decoded CSI arrays only;
it is **not** part of the streaming path.

Scope decision: **signal processing only.** DWT (wavelet transforms) and
PCA/compression are explicitly deferred to a later phase. Integration with the
`visualization` module is a future plan (see "Out of scope / Future work").

## Design principles

1. **One decoder, many consumers.** `dsp` must not re-parse `.bin` files. It consumes
   arrays produced by `saving` (timestamps, amplitude/phase or complex I/Q). A thin
   convenience loader may wrap `saving`'s output, but no second decode path.
2. **Synchronous and pure.** No tokio, no network, no file I/O inside the DSP
   functions themselves. All functions are deterministic transforms over
   `ndarray` arrays. This keeps the module usable (or ignorable) from `streaming`
   later without coupling.
3. **Composable core type.** Introduce `CsiSignal`:

   ```rust
   pub struct CsiSignal {
       /// Amplitude, shape (n_samples, n_subcarriers)
       pub amplitude: Array2<f64>,
       /// Raw (unsanitized) phase in radians, same shape. May be empty/optional.
       pub phase: Array2<f64>,
       /// Per-sample device timestamps (microseconds), length n_samples.
       pub ticks_us: Array1<u64>,
       /// Nominal sample rate (Hz), derived from median tick interval.
       pub fs: f64,
     }
   ```

   Every DSP operation takes/returns this type (or `ArrayView` views of it), so the
   pipeline chains naturally.
4. **Errors.** `DspError` enum (mirrors `CollectionError` style): `NonMonotonicTicks`,
   `EmptySignal`, `InvalidShape`, `ZeroVariance`, `InvalidConfig`, etc. No
   `Box<dyn Error>` in public signatures.
5. **Documentation.** Every public function gets rustdoc with a runnable or
   `no_run` `# Example` block, following the existing `collection.rs` style.
   A usage section in the module docs shows the full chain:
   decode → normalize → resample → filter → FFT/STFT.

## Dependency policy

External crates are **allowed and preferred** when the needed function already
exists there, especially when a crate is faster or better-validated than a
from-scratch implementation. Deciding rule: use a crate if it is maintained,
adds the function directly, and does not drag in async/networking machinery;
otherwise hand-roll and unit-test against synthetic signals.

Current and candidate dependencies:

| Crate | Status | Purpose |
| --- | --- | --- |
| `rustfft` | already in `Cargo.toml` (6.4.1) | core FFT/STFT engine; reuse `FftPlanner` |
| `ndarray`, `ndarray-npy` | already in `Cargo.toml` | arrays and `.npy`/`.npz` glue |
| `realfft` | candidate | real-input FFT wrapper around rustfft; roughly 2× faster than complex FFT for real signals and returns the one-sided spectrum directly. Preferred for `spectral.rs` if it slots in cleanly; otherwise stay with `rustfft` + manual one-sided reduction. |
| `biquad` | candidate | standard biquad filters (LPF/HPF/BPF). Preferred over hand-rolling RBJ cookbook code if the API fits per-subcarrier column-wise processing; otherwise a small hand-rolled biquad module is acceptable. |
| cubic spline | candidate | evaluate maintained crates (e.g., a `spline`/interpolation crate) for `CubicSpline` resampling; fall back to a hand-rolled natural cubic spline (small, well-testable) if no crate is a good fit. |
| `ndarray-stats` | candidate | median/percentile helpers for Hampel and median filters, if API fits; otherwise hand-roll (selection is small for typical window sizes). |

Decision points (resolve at implementation time, document the choice in the
module docs):

1. `realfft` vs plain `rustfft` for real-valued CSI series.
2. `biquad` crate vs ~50-line hand-rolled RBJ biquad.
3. Spline crate vs hand-rolled natural cubic spline.

Anything not on this list (DWT, PCA, wavelets) is out of scope for this rewrite
and would revisit this table when scheduled.

## Module layout

Split the rewrite into submodules under `src/dsp/` (or clearly-sectioned single file
if preferred, but submodules recommended once filters land):

```
src/dsp/
  mod.rs         // CsiSignal, DspError, module docs, re-exports
  preprocess.rs  // normalization, CSI-ratio (conjugate multiplication)
  resample.rs    // interpolation to uniform sample rate
  spectral.rs    // FFT, STFT, windowing, frequency helpers
  filter.rs      // outlier removal + biquad frequency-domain filters
```

## Features

### 1. Decode integration (thin)

- `CsiSignal::from_npz(path)` or `CsiSignal::from_capture(path)` reusing `saving`'s
  decode output (amplitude, phase, `device_ticks_us`).
- `fs` computed from median tick interval (logic already exists in the old
  `main` example; move it here).
- Validate: non-empty, ticks non-decreasing, even column count for I/Q formats.
- Phase is documented as **raw and unsanitized**; amplitude is the primary signal.

### 2. Normalization (per subcarrier)

- `normalize(&self, scheme: NormScheme) -> CsiSignal`
- `NormScheme` enum:
  - `PerSubcarrier` (default): per-subcarrier mean removal / std division, with
    epsilon guard `(σ + ε)` against near-zero variance.
  - `PerSample`: whole-matrix normalization (CSI-Bench style).
  - `MinMax`: per-subcarrier min-max scaling.

### 3. Resampling / interpolation

- `resample(&self, target_fs: f64, method: InterpMethod) -> Result<CsiSignal>`
- **Interpolate over the whole record at once**, on a uniform time grid derived
  from cumulative tick timestamps. Do NOT chunk per second: per-second independent
  interpolation creates boundary artifacts (kinks at each second edge, spectral
  leakage into STFT).
- Interpolate the underlying **I/Q (complex) values** (or I and Q separately),
  then recompute amplitude/phase. Never interpolate amplitude directly, and never
  interpolate raw phase (wrap-around discontinuities).
- `InterpMethod` enum: `Linear`, `Nearest` ("piecewise"), `CubicSpline`.
  Cubic spline is the recommended default for spectral work.
- Non-monotonic / duplicate ticks: drop duplicates, error (or strictly monotone
  interpolation) on regressions — decide once and test it.

### 4. FFT

- `fft(signal: &[f64], fs: f64, window: Window) -> Spectrum`
  - `Spectrum { freqs: Array1<f64>, magnitude: Array1<f64> (or complex) }`
  - One-sided spectrum (bins `1..n/2`), correct amplitude scaling
    (factor `2/window_sum`, as in the current Hann implementation).
- `Window` enum: `None`, `Hann` (default), `Hamming`, `Blackman`.
- `freq_bins(n, fs)` helper.
- Convenience wrapper: `dominant_peaks(spectrum, min_hz, max_hz, top_k) -> Vec<(f64, f64)>`
  — replaces the hardcoded 0.2–10 Hz band in the old `fft_peaks`; band is
  caller-specified (e.g., 0.1–0.5 Hz breathing, 0.2–10 Hz motion).
- Per-subcarrier convenience: `fft_subcarriers(&self, window) -> Vec<Spectrum>`.

### 5. STFT

- `stft(signal: &[f64], fs: f64, config: StftConfig) -> Spectrogram`
  - `StftConfig { window_len: usize (samples), hop: usize, window: Window }`
    with defaults: Hann, 50% overlap.
  - `Spectrogram { times: Array1<f64>, freqs: Array1<f64>, magnitudes: Array2<f64> }`
    (freq × time frame), magnitudes in dB optional flag or helper `to_db()`.
- One planned FFT reused across frames and subcarriers (`FftPlanner` outside loops).
- Document window-length guidance: long window → good frequency resolution,
  short → good time resolution (survey trade-off). Suggested defaults in docs:
  ~10–20 s for breathing at 30 Hz, 0.5–1 s for gestures/motion.

### 6. Outlier removal (added per review)

- `moving_average(&[f64], k)` — with WMA/EWMA variants if trivial.
- `median_filter(&[f64], k)`.
- `hampel_filter(&[f64], window, n_sigma)` — window median/σ, replace or drop
  outliers; the single most-used CSI outlier remover in the survey literature.
- All operate per subcarrier (column-wise) with small boundary handling (shrink
  window at edges; document the choice).

### 7. Simple frequency-domain filters (added per review)

- Biquad low-pass / high-pass / band-pass:
  `lowpass(&[f64], fs, cutoff_hz)`, `highpass(...)`, `bandpass(..., lo_hz, hi_hz)`.
- External crates allowed and preferred when they fit (see "Dependency policy"):
  evaluate `biquad` first; fall back to a hand-rolled RBJ cookbook biquad only
  if its API does not fit column-wise processing.
- Typical use documented: BPF 0.1–0.5 Hz for breathing isolation; HPF to remove
  static-object (low-frequency) reflections before motion/gesture analysis.

### 8. Optional: CSI-ratio helper

- `csi_ratio(&self) -> CsiSignal` or a phase-preprocessing function computing
  `h[t] · conj(h[t-1])` per subcarrier (needs the complex data retained from
  decoding). Standard phase-insensitive preprocessing on ESP32 platforms.
  Mark as optional; if deferred, keep phase output documented as raw.

## Testing (synthetic data only)

Unit tests in each submodule, all on synthetic signals — no captures, no fixtures:

- **Normalization**: constant subcarrier (near-zero σ) does not panic thanks to
  ε guard; per-subcarrier output has zero mean, unit std per column.
- **Resampling**: known linear ramp and sine at 100 Hz resampled to/from rates
  recovers expected values within tolerance; `fs` field updated; spline vs linear
  accuracy comparison on a smooth curve.
- **FFT**: pure sine at f Hz → dominant peak at f (± one bin) for f in a sweep;
  Hann amplitude scaling correctness against analytic amplitude; DC removal.
- **STFT**: constant-frequency sine → flat ridge at f; chirp → ridge tracking the
  instantaneous frequency; `times`/`freqs` axes match config.
- **Outlier removal**: signal + injected spikes (impulse noise, ±outliers) →
  Hampel/median recover the clean signal within tolerance.
- **Filters**: LPF on sine + high-frequency interference removes the interference
  band; BPF passes the in-band tone and attenuates out-of-band (check in dB).
- **Integration test**: synthetic complex signal → full chain
  (resample → normalize → hampel → stft) produces consistent shapes and axes.

## Docs

- Module-level docs with a pipeline overview diagram (ASCII) and one
  end-to-end `no_run` example.
- Per-function `# Example` blocks (compile-tested via `cargo test --doc`).
- A short "How to choose" table: breathing → BPF 0.1–0.5 Hz + FFT peaks;
  motion/gesture → Hampel + HPF + STFT; general cleanup → Hampel → normalize.

## Out of scope / Future work

- **DWT (wavelet transform)** and **PCA/compression** — deferred, revisit later.
- **Integration with the `visualization` module** — planned follow-up once the core
  API stabilizes (e.g., rendering `Spectrogram` as a heatmap alongside the existing
  amplitude dashboard). Keep `Spectrogram`/`Spectrum` plain-data structs so the
  visualization layer can consume them without depending on dsp internals.
- Streaming integration — explicitly not part of this module's responsibility.

## Implementation order

1. `mod.rs`: `CsiSignal`, `DspError`, loader glue, module docs.
2. `preprocess.rs`: normalization (+ optional CSI-ratio).
3. `resample.rs`: whole-record interpolation.
4. `spectral.rs`: `Window`, `freq_bins`, FFT, `dominant_peaks`.
5. `spectral.rs`: STFT + `Spectrogram`.
6. `filter.rs`: Hampel/median/MA outlier removal.
7. `filter.rs`: biquad LPF/HPF/BPF.
8. Synthetic tests throughout (write with each step, not after).
9. Deprecation cleanup: delete the old `dsp.rs` functions once superseded
   (`load_csi`, `load_ticks`, `mean_std`, `amplitude`, `fft_peaks` — `amplitude`
   logic moves into the decode glue).
