# DSP module

Source: [`src/dsp/mod.rs`](../src/dsp/mod.rs) (+ `preprocess.rs`, `resample.rs`,
`spectral.rs`, `filter.rs`) · No standalone binary (library only)

Synchronous signal processing for decoded CSI records. The module never parses
`.bin` files itself — it consumes arrays produced by the
[decoder](decoder.md)/[saving](saving.md) pipeline through one core type,
[`CsiSignal`](#csisignal). All DSP functions are pure, deterministic transforms
over `ndarray` arrays: no tokio, no network, no file I/O inside the transforms
(the only I/O is in the two thin loaders).

```text
capture_v8.bin ──decoder──▶ CsiPacket[] ──▶ CsiSignal::from_capture
capture_v8.npz ──saving───┘                  │
                                               ▼
                              normalize ──▶ resample ──▶ hampel ──▶ bandpass
                                               │
                                     ┌─────────┴─────────┐
                                     ▼                   ▼
                              fft / dominant_peaks    stft → Spectrogram
```

## `CsiSignal`

```rust
pub struct CsiSignal {
    pub amplitude: Array2<f64>,              // (n_samples, n_subcarriers)
    pub phase: Array2<f64>,                  // raw, unsanitized, radians
    pub ticks_us: Array1<u64>,               // device_ticks_us, non-decreasing
    pub fs: f64,                             // nominal sample rate, Hz
    pub iq: Option<Array2<Complex<f64>>>,    // h_k(t) = I + jQ when available
    pub null_bins: Vec<usize>,               // subcarrier indices with no energy
}
```

`null_bins` lists the subcarriers the standard defines as unused — for the
HT20 preamble these are the DC bin (32), the three guard bins on each edge
(0–3 and 61–63), giving 8 null bins out of 64. They carry no channel
information: amplitude is a hardware artefact, phase is meaningless noise.
Downstream code skips them whenever a statistic is computed across
subcarriers. `from_packets` fills the list with `null_bins_for(n_subcarriers)`,
which returns the HT20 table when `n_subcarriers == HT20_SUBCARRIERS` (64) and
an empty list otherwise — a different standard needs a different table, so the
list lives in one place instead of being hard-coded per call site.

For subcarrier $k$ at sample $t$, with $h_k(t) = I_k(t) + j\,Q_k(t)$:

$$|h_k(t)| = \sqrt{I_k(t)^2 + Q_k(t)^2}, \qquad
   \varphi_k(t) = \operatorname{atan2}\bigl(Q_k(t), I_k(t)\bigr)$$

- **Amplitude is the primary signal.** Phase is stored raw and wrapped to
  $[-\pi, \pi]$ — never unwrapped or smoothed.
- **`fs` is derived from the median tick interval:**

  $$f_s = \frac{10^6}{\operatorname{median}(\Delta\, ticks\_us)}$$

- **Resampling interpolates I/Q, not amplitude or raw phase.** Interpolating
  amplitude distorts envelopes; interpolating raw phase breaks on the
  $\pm\pi$ wrap-around. Interpolating $I$ and $Q$ separately avoids both, then
  amplitude/phase are recomputed.

### Loading

| Function | Input | Notes |
|---|---|---|
| `CsiSignal::from_capture(path)` | raw `.bin` | thin wrapper over the one decode path (`decoder` via `saving`); fills `iq` from the wire |
| `CsiSignal::from_npz(path)` | `.npz` from [`saving`](saving.md) (`timestamp_ms`, `amplitude_XX`, `phase_XX`) | reconstructs ticks and I/Q exactly from amplitude+phase |
| `CsiSignal::from_packets(&[CsiPacket])` | decoded packets | for streaming-adjacent callers that already hold packets |
| `CsiSignal::new(amplitude, phase, ticks_us)` | arrays | validates shapes, monotonic ticks, computes `fs` |
| `CsiSignal::from_iq(iq, ticks_us)` | complex array | derives amplitude/phase |

Validation: non-empty, matching shapes, non-decreasing `ticks_us`
(`DspError::NonMonotonicTicks`), and at least one positive tick interval
(`DspError::ZeroVariance` otherwise).

```rust
use std::path::Path;
use rt_wireless_sensing::dsp::{CsiSignal, DspError, InterpMethod, NormScheme, Window, fft, dominant_peaks};

let signal = CsiSignal::from_capture(Path::new("artifacts/capture_v8.bin"))?;
let signal = signal.normalize(NormScheme::PerSubcarrier);
let signal = signal.resample(20.0, InterpMethod::CubicSpline)?;
let signal = signal.hampel(5, 3.0)?;
let signal = signal.bandpass(0.1, 0.5)?; // breathing band
for sc in 0..signal.n_subcarriers() {
    let col = signal.column(sc);
    let spectrum = fft(col.as_slice().unwrap(), signal.fs, Window::Hann)?;
    let peaks = dominant_peaks(&spectrum, 0.1, 0.5, 3);
    println!("subcarrier {sc}: {peaks:?}");
}
```

## Preprocessing — `src/dsp/preprocess.rs`

### `normalize(NormScheme)`

| Scheme | Formula |
|---|---|
| `PerSubcarrier` (default) | $z_{t,k} = \dfrac{x_{t,k} - \mu_k}{\sigma_k + \varepsilon}$ |
| `PerSample` (CSI-Bench style) | $z = \dfrac{x - \mu}{\sigma + \varepsilon}$ over the whole matrix |
| `MinMax` | $z_{t,k} = \dfrac{x_{t,k} - \min_k}{\max_k - \min_k + \varepsilon}$ |

The epsilon guard $\varepsilon = 10^{-12}$ keeps constant (zero-variance)
subcarriers finite instead of dividing by zero. Only `amplitude` is
transformed; phase, ticks, `fs`, and the complex view stay consistent.

**Null subcarriers are excluded from every statistic.** `PerSubcarrier` and
`MinMax` skip the null columns when computing $\mu_k, \sigma_k, \min_k, \max_k$
and leave those columns at 0. `PerSample` computes the single global $\mu,
\sigma$ over the non-null values only, then writes 0 into the null columns.
Scoring them would let the near-zero guard bins drag the mean down and dilute
the variance for every real subcarrier. With an empty `null_bins` the output is
bit-identical to the plain implementation.

### `csi_ratio` (optional)

Conjugate product per subcarrier — the standard phase-insensitive
preprocessing on ESP32 platforms. The static phase offset cancels:

$$r_k(t) = h_k(t)\, h_k^*(t-1)
        = |h_k(t)|\,|h_k(t-1)|\; e^{\,j(\varphi_k(t) - \varphi_k(t-1))}$$

The first sample has no predecessor and is set to $r_k(0) = |h_k(0)|^2$, so the
output keeps the input length. Amplitude/phase of the result are recomputed
from the ratio.

## Resampling — `src/dsp/resample.rs`

`resample(target_fs, method)` interpolates the **whole record at once** onto a
uniform grid $u_j = j / f_s^{target}$ (derived from cumulative tick times), then
recomputes amplitude/phase from the interpolated I/Q. Whole-record (not
per-second) interpolation avoids boundary kinks and STFT spectral leakage.

Duplicate ticks are dropped (first occurrence wins); a tick regression is a
`DspError::NonMonotonicTicks` error.

| `InterpMethod` | Behavior |
|---|---|
| `Nearest` | piecewise-constant (nearest source sample) |
| `Linear` | piecewise-linear between surrounding samples |
| `CubicSpline` (default, recommended for spectral work) | natural cubic spline, $S''(t_0) = S''(t_{n-1}) = 0$, solved with the Thomas algorithm |

The cubic spline is hand-rolled (~40 lines, unit-tested against analytic
curves): no maintained crate evaluates a *non-uniform* natural cubic spline —
the `splines` crate's Catmull-Rom requires uniform spacing, which jittered
device ticks violate.

### Gap handling — `resample_with`

`resample()` is a thin wrapper over `resample_with(target_fs, method, options)`,
the single implementation. The `options` variant additionally reports *how*
each grid point was produced:

```rust
pub struct ResampleOptions { pub max_gap_us: Option<u64> }
pub struct ResampleOutput {
    pub signal: CsiSignal,
    pub grid_state: Vec<GridState>,        // one entry per grid column
    pub nominal_interval_us: f64,
}
pub enum GridState { Observed = 0, Bridged = 1, Masked = 2 }
```

A dropout longer than one sample interval is not a signal feature, so the
source is **split into segments at every over-threshold gap** and each segment
is interpolated independently; adjacent segments share their boundary sample so
no data is lost. A grid column is only written when it falls inside a segment —
the gap itself is never extrapolated across, which is what prevents a spline
from inventing a smooth bridge over a 250 ms dropout.

Each written column is then classified against `nominal_interval_us` (the median
source interval):

| `GridState` | Condition | Meaning |
|---|---|---|
| `Observed` | source gap $\le 1.5 \times$ median interval | real data, only interpolated for jitter |
| `Bridged` | between the 1.5× limit and `max_gap_us` | interpolated across a genuine dropout — plausible but reconstructed |
| `Masked` | gap $> $ `max_gap_us` | withheld: too long to trust, flagged for analysis |

With `max_gap_us == None` nothing is masked, but `Observed`/`Bridged` are still
distinguished. Downstream consumers render `Masked` as NaN (or an empty CSV
cell) rather than as a number, so a visible hole never looks like a measurement.

## Spectral analysis — `src/dsp/spectral.rs`

Real-input FFTs use the [`realfft`](https://crates.io/crates/realfft) crate
(one-sided spectrum directly, ~2× faster than a complex FFT, shares
`rustfft` 6.4). For a real signal of length $N$ at sample rate $f_s$:

$$X_k = \sum_{n=0}^{N-1} w_n\, x_n\, e^{-j 2\pi k n / N}, \qquad
   f_k = \frac{k f_s}{N}, \quad k = 0 \dots \lfloor N/2 \rfloor$$

### `fft(signal, fs, window) -> Spectrum`

Removes the mean (DC), applies the window $w_n$, and returns the one-sided
spectrum for bins $k = 1 \dots \lfloor N/2 \rfloor$ with amplitude scaling

$$|X_k| \mapsto \frac{2\,|X_k|}{\sum_n w_n}$$

(the Nyquist bin of an even-length signal is not doubled). With a Hann window
$\sum w_n \approx N/2$, so a pure sine of amplitude $A$ peaks at $\approx A$.

| `Window` | Formula (periodic, denominator $N$) |
|---|---|
| `None` | $w_n = 1$ |
| `Hann` (default) | $w_n = 0.5 - 0.5\cos(2\pi n / N)$ |
| `Hamming` | $w_n = 0.54 - 0.46\cos(2\pi n / N)$ |
| `Blackman` | $w_n = 0.42 - 0.5\cos(2\pi n/N) + 0.08\cos(4\pi n/N)$ |

- `Spectrum { freqs, magnitude, complex }` — `freqs[k] = (k+1) f_s / N`.
- `freq_bins(n, fs)` — one-sided bins $f_k = k f_s / N$, $k = 0..\lfloor N/2\rfloor$.
- `dominant_peaks(&spectrum, min_hz, max_hz, top_k)` — top-`k` `(freq, magnitude)`
  pairs in a caller-specified band (replaces the old hardcoded 0.2–10 Hz):
  0.1–0.5 Hz for breathing, 0.2–10 Hz for motion.
- `signal.fft_subcarriers(window)` — per-subcarrier spectra.

### `stft(signal, fs, &StftConfig) -> Spectrogram`

Slides a window with a hop and stacks one-sided frame spectra:

$$S(m, k) = \sum_{n=0}^{W-1} w_n\, x_{mH+n}\, e^{-j 2\pi k n / W}$$

- `StftConfig { window_len, hop, window }` — defaults: Hann, 50% overlap
  (`hop = window_len / 2`); `StftConfig::with_window(len, window)` for the
  common case.
- `Spectrogram { times, freqs, magnitudes }` — `magnitudes` is
  `(n_freqs, n_frames)` (frequency × time); `times[j] = (start_j + W/2)/f_s`
  is the frame center in seconds. `to_db()` converts magnitudes with
  $20\log_{10}(\max(m, 10^{-12}))$.
- One planned FFT is reused across all frames and subcarriers.
- Window-length guidance: long window → good frequency resolution, short →
  good time resolution.

| Signal of interest | Window length | At 20 Hz sampling |
|---|---|---|
| Breathing (0.1–0.5 Hz) | 10–20 s | 200–400 samples |
| Motion / gestures (0.2–10 Hz) | 0.5–1 s | 10–20 samples |

- `signal.stft_subcarriers(&config)` — per-subcarrier spectrograms.

## Filtering — `src/dsp/filter.rs`

### Outlier removal (time domain, per subcarrier)

All use a centered window of `k` samples (rounded up to odd), shrunk at the
edges.

| Function | Formula |
|---|---|
| `moving_average(x, k)` | $y_i = \frac{1}{|W_i|}\sum_{j \in W_i} x_j$ (O(n) via prefix sums) |
| `exp_moving_average(x, k)` | $y_i = \alpha x_i + (1-\alpha) y_{i-1}$, $\alpha = 2/(k+1)$ |
| `median_filter(x, k)` | $y_i = \operatorname{median}(W_i)$ |
| `hampel_filter(x, window, n_sigma)` | replace $x_i$ with $m_i$ when $\|x_i - m_i\| > n_\sigma\, \sigma_i$ |

Hampel is the most-used CSI outlier remover in the survey literature, with
robust (MAD-based) deviation:

$$m_i = \operatorname{median}(W_i), \qquad
   \sigma_i = 1.4826 \cdot \operatorname{median}_{j \in W_i} |x_j - m_i|$$

Medians are hand-rolled with quickselect (`select_nth_unstable`): the
`ndarray-stats` 0.7 quantile API is in-place on `ArrayRef` and does not fit
read-only sliding windows over `&[f64]`, and window sizes are small.

### Frequency-domain biquads

`lowpass`, `highpass`, `bandpass` apply second-order RBJ biquads via the
[`biquad`](https://crates.io/crates/biquad) crate (`DirectForm2Transposed`,
Butterworth $Q = 1/\sqrt{2}$):

$$y[n] = b_0 x[n] + b_1 x[n-1] + b_2 x[n-2] - a_1 y[n-1] - a_2 y[n-2]$$

`bandpass(x, fs, lo, hi)` is a high-pass at `lo` cascaded with a low-pass at
`hi`. Note the startup transient (the first few samples are not steady-state).
Typical use: BPF 0.1–0.5 Hz for breathing isolation; HPF above ~0.2 Hz to
remove static-object reflections before motion/gesture analysis.

Each slice function has a `CsiSignal` method that applies it to every amplitude
subcarrier column: `signal.hampel(5, 3.0)`, `signal.lowpass(5.0)`,
`signal.bandpass(0.1, 0.5)`, …

## How to choose

| Goal | Suggested chain |
|---|---|
| Breathing / vital signs (0.1–0.5 Hz) | `bandpass(0.1, 0.5)` → `fft` + `dominant_peaks` |
| Motion / gestures (0.2–10 Hz) | `hampel` → `highpass(0.2)` → `stft` |
| General cleanup | `hampel` → `normalize(PerSubcarrier)` |
| Comparing records with different packet rates | `resample(fs, CubicSpline)` first |

## Errors

`DspError` is a concrete enum (same style as `CollectionError`/`SaveError`), no
`Box<dyn Error>` in public signatures:

| Variant | Cause |
|---|---|
| `Io` / `Decode` / `Npz` | file, capture, or archive failure (wrapped sources) |
| `MissingArray(name)` | required array absent from an `.npz` |
| `EmptySignal` | zero samples |
| `InvalidShape(msg)` | inconsistent array shapes |
| `NonMonotonicTicks { index, previous, found }` | ticks moved backwards |
| `ZeroVariance(msg)` | e.g. all tick intervals zero, so no sample rate exists |
| `InvalidConfig(msg)` | bad `fs`, window size, cutoff, band, … |

## Out of scope (future work)

- DWT/wavelets and PCA/compression — deferred.
- Integration with [`visualization`](visualization.md) — planned once the core
  API stabilizes; `Spectrum`/`Spectrogram` are plain-data structs so a renderer
  can consume them without dsp internals.
- Streaming integration — explicitly not this module's responsibility.

## Testing

All tests are synthetic (no captures, no fixtures): unit tests in each
submodule (normalization epsilon guard, spline vs linear accuracy on a smooth
curve, FFT peak within one bin and Hann amplitude recovery, STFT ridge of a
sine/chirp, Hampel/median spike recovery, LPF/BPF attenuation in dB) plus an
integration test in [`tests/dsp.rs`](../tests/dsp.rs) running the full chain
`resample → normalize → hampel → bandpass → stft` and an `.npz` round-trip in
the `saving` layout. Doc examples are compile-tested (`cargo test --doc`).
