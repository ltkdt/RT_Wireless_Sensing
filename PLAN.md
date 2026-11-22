# plan.md — DSP view (resample + normalize) for offline visualization

Status: draft — references verified against HEAD `2cd4d0e` (2026-11-21).
Replaces the previous `PLAN.md` (opt-in diagnostic trace), which is implemented and
recorded in `Memory.md` §2.6; it stays in git history.

## Goal

Add an optional **DSP view** to the visualization: panels computed by the `dsp` module
from the same decoded packets as the heatmap. The first implementation covers exactly
two DSP features:

1. **Resampling** onto a uniform grid at the intended PPS (post-interpolation amplitude
   and phase — the default view, before any other transformation).
2. **Normalization** (`NormScheme`), shown as an additional amplitude panel when selected.

Constraints: streaming is never affected; the web layer stays vanilla JS + Canvas 2D with
**no new libraries**; Rust stays the single implementation of every formula.

## Decisions (settled)

| # | Topic | Decision |
|---|-------|----------|
| D1 | Streaming | No DSP at all. The payload carries `dsp: null` and the web layer shows a note: *"DSP analysis is available after streaming is closed."* This is an **intentional exception** to the project-wide opt-in rule (`Memory.md` L168–191): that rule anticipated a `--dsp` streaming flag; it is replaced by this decision. |
| D2 | Offline delivery (**Snapshot**) | DSP parameters are fixed on the `visualize` CLI. The result is embedded in the `VisualizationData` payload, so the standalone HTML and `--serve` show the same precomputed data. No on-demand endpoint. |
| D3 | Future | Compile the `dsp` module to **WASM** so a shared HTML can change interpolation/transform interactively (one implementation, no JS port). Out of scope. |
| D4 | Gaps | By default **no time threshold**: every gap is interpolated. An optional CLI argument sets a maximum gap; longer gaps are masked. |
| D5 | Null subcarriers | Rendered **masked** (never 0). Excluded from any transform that works across the subcarrier axis (e.g. whole-matrix normalization). The DSP panel has an option to **hide** them. |
| D6 | Time window | A time-window selector (slider) is the default for **every time-based canvas**. Maximum visible window: **5 minutes**. |
| D7 | Export | **CSV**, generated in the browser; no libraries. |
| D8 | First scope | Resampling (default target = intended PPS) and normalization only. FFT/STFT/filters are later. |

## Current state (verified)

**Payload / Rust** (`src/visualization.rs`)
- `VisualizationData { metadata, timestamps, amplitudes: Vec<Vec<f32>>, classification, diagnostics: Option<_> }` — :394. Amplitudes come from `packet.amplitudes()` (:1166).
- `VisualizationOptions { nominal_pps, capture_end, diagnostics }` — :1030 (`Copy`, `PartialEq`); `Default` :1054.
- `prepare_visualization_data_with_options` — :1115; trace gated at :1203; `Ok(VisualizationData {…})` at :1208.
- Offline entry `from_bytes_with_pps` builds the options literal — :1243.
- `render_html_bundle` inlines `window.CSI_DATA` — :1293. `serve_dashboard` serializes **once** (`data.to_json()`) and serves it at `/api/data` — :1327. Live variants — :1403, :1420.
- Streaming builds the options literal — `src/streaming.rs:484-488` (`StreamingConfig` :66).
- CLI: `USAGE` `src/bin/visualize.rs:17`, `Options` :42, `parse_args` :149, `--pps` :177 (feeds `nominal_pps`).

**DSP** (`src/dsp`)
- `CsiSignal { amplitude, phase, ticks_us, fs, iq }` — `mod.rs:140`. `from_packets` (:399) already fills `iq`; I/Q is Q-first on the wire and `iq_pairs()` implements it (`Memory.md` §5; verified against the ESPectre reference). No decoder/format change is needed.
- `resample(target_fs, InterpMethod)` — `resample.rs:83`. Whole record at once, interpolates **I and Q** (never amplitude or raw phase), grid `u_j = j / target_fs` anchored at the first tick, spans `[0, duration]` (no extrapolation). `InterpMethod`: `Nearest`, `Linear`, `CubicSpline` (default; natural spline over **all** knots). No gap handling.
- `normalize(NormScheme)` — `preprocess.rs:67`: `PerSubcarrier` (default), `PerSample` (whole matrix), `MinMax`. Transforms **amplitude only**; phase is untouched.
- Interpolation is on the **time axis only**; nothing interpolates across subcarriers.
- Null bins are `{0,1,2,3,32,61,62,63}` (centered HT20, 64 bins; `Memory.md` §5/§7). `saving` keeps them as zero amplitude/phase.

**Web** (`web/visualizer.js`, `web/index.html`)
- `CsiVisualizer.setData` :107, `updateOffscreenBuffer` :277 (one pixel column per frame), `render` :311, `drawAxes` :376, `eventToPoint` :569, `initEvents` :637.
- `DiagnosticView` :794 — gated on its payload field (:847); toggle bar `index.html:94-101`, section :103.
- Bootstrap :1586; `applyPayload` :1616 distinguishes live (`status: "ready"`, :1630), static server (:1638) and inlined bundle (:1645).
- PNG export already uses a generated `<a download>` link, so CSV can follow the same pattern.
- `Cargo.toml` has no base64 crate; `serde_json` uses `float_roundtrip`.

## Behavior after the change

| Mode | `dsp` in payload | UI |
|------|------------------|----|
| Streaming (`streaming` bin) | always `null` | toggle replaced by the D1 note |
| Offline `visualize` (HTML bundle and `--serve`) | `Some(…)`, computed once | DSP toggle + panels |
| Old payload without the field | `None` (`#[serde(default)]`) | toggle hidden, no note |

The web layer shows the note only when `applyPayload` took the live branch (:1630) and
`dsp` is null; it passes that as a flag into `setData`. Nothing is recomputed in the
browser (project rule: payload switch, not web switch).

## Payload

```rust
pub struct DspView {
    pub interp: String,                 // "nearest" | "linear" | "cubic_spline"
    pub target_fs: f64,                 // = slots_per_second (see Changes §2)
    pub norm: Option<String>,           // None when no normalization was requested
    pub max_gap_ms: Option<f64>,        // None = no threshold
    pub timestamps: Vec<f64>,           // resampled grid, seconds from the first record
    pub null_subcarriers: Vec<usize>,   // bins to mask / exclude
    pub time_state: Vec<u8>,            // per grid column: 0 observed, 1 bridged, 2 masked
    pub panels: Vec<DspPanel>,
}
pub struct DspPanel {
    pub id: String,                     // "amplitude" | "phase" | "amplitude_norm"
    pub title: String,
    pub unit: String,                   // "a.u." | "rad" | "z"
    pub kind: String,                   // "sequential" | "cyclic"
    pub data: Vec<Vec<f32>>,            // [time][subcarrier], like `amplitudes`
}
```

- The mask is **separable**: gaps are per time column (`time_state`), nulls per subcarrier
  (`null_subcarriers`), so no per-cell mask matrix is shipped.
- `time_state`: *observed* = both source neighbours within 1.5 nominal intervals;
  *bridged* = interpolated across a longer gap; *masked* = gap longer than `max_gap_ms`.
- Panels: `amplitude` and `phase` (resampled) always; `amplitude_norm` only when a
  normalization is chosen. Phase has no normalized counterpart (`normalize` leaves it alone).
- Matrices stay plain JSON arrays: consistent with `amplitudes`, readable in `--json`
  exports, no new dependency. Reference size ≈ 9 B per value, i.e. ~0.4 MB per matrix
  for the 34 s / 20 PPS capture. Base64 typed arrays (~40% smaller) are an option if size
  becomes a problem.

## Changes

### 1. `dsp`
1. **Null bins.** Add `pub const NULL_BINS_HT20: [usize; 8] = [0,1,2,3,32,61,62,63]` and a
   `null_bins: Vec<usize>` field on `CsiSignal` (`mod.rs:140`), empty by default and set by
   `from_packets` for 64-bin captures. Only in-crate literal: `csi_ratio`
   (`preprocess.rs:153`) — carry the field over; `with_amplitude` keeps it.
2. **Normalization.** `normalize` (`preprocess.rs:67`): for `PerSample`, compute μ/σ over
   non-null columns only; null columns are written as 0. `PerSubcarrier` and `MinMax` are
   per column, so only their null columns need forcing to 0 (already the result via ε).
   With `null_bins` empty, output is bit-identical to today.
3. **Gap threshold.** Add `resample_with(target_fs, method, ResampleOptions { max_gap_us: Option<u64> })`
   returning the signal plus a per-grid-column state (`observed | bridged | masked`).
   `resample` stays as a thin wrapper (existing tests and `docs` examples unchanged).
   When a threshold is set, **split the source series at over-threshold gaps and
   interpolate each segment independently**: the natural cubic spline is a global fit, so
   masking afterwards would let a masked gap distort its neighbours. With no threshold the
   whole-record rule (`Memory.md` §2.5) is untouched.

### 2. `visualization.rs`
1. Types `DspView`, `DspPanel`; `VisualizationData.dsp: Option<DspView>` with
   `#[serde(default)]` (old payloads parse).
2. `VisualizationOptions.dsp: Option<DspOptions>` (`:1030`, stays `Copy`), with
   `DspOptions { interp: InterpMethod, norm: Option<NormScheme>, max_gap_ms: Option<f64> }`.
   Offline default = `Some(default)` (offline is exempt from the opt-in rule: computed once);
   streaming = `None`.
3. In `prepare_visualization_data_with_options` (after :1203): when `dsp` is `Some`, build
   `CsiSignal::from_packets`, `resample_with(slots_per_second as f64, …)`, optional
   `normalize`, and fill `DspView`. Target = `slots_per_second` (`nominal_pps.round()`), so
   resampled column `j` ↔ diagnostic slot `(second = j / N, slot = j % N)`.
4. Update the options literals: `streaming.rs:484` (`dsp: None`), `visualization.rs:1243`,
   `tests/visualization.rs` (`prepare_with`, ~:252).

### 3. CLI (`src/bin/visualize.rs`; names TBD)
- `--dsp-interp nearest|linear|cubic` (default `cubic`)
- `--dsp-norm none|per-subcarrier|per-sample|min-max` (default `none`)
- `--dsp-max-gap-ms MS` (default: no threshold)
- `--no-dsp` (skip the block to keep the file small)
- Extend `USAGE` (:17), `Options` (:42), `parse_args` (:149) and its unit tests. No change to `src/bin/streaming.rs`.

### 4. Web
1. `index.html`: DSP toggle bar and section next to the diagnostic ones (:94-103);
   note element for live mode.
2. New `DspView` class mirroring `DiagnosticView` (`setData` / `setEnabled` / `render`),
   gated on `data.dsp`; amplitude and phase canvases stacked, sharing the heatmap's plot
   margins and the crosshair readout.
3. Phase panel: **cyclic colormap** (new LUT beside `COLORMAPS`) with a −π…π colorbar.
4. Null subcarriers: masked rows (hatched, as the diagnostic grid does for empty slots);
   checkbox **"Hide null subcarriers"** drops those rows, y-axis ticks keep physical bin indices.
5. `time_state` drawn as a thin strip above the panels (observed / bridged / masked).
6. `applyPayload` (:1616): pass `live` into `setData` from the live branch only.

### 5. Time-window selector (separate change)
- Shared window state `{start, length}` in `CsiVisualizer`; `MAX_WINDOW_S = 300`.
- `updateOffscreenBuffer` (:277) builds the bitmap for the **window only** — this removes
  the canvas-size limit regardless of recording length (5 min at 100 Hz ≈ 30k columns).
- Map window ↔ canvas in `drawAxes` (:376), `eventToPoint` (:569) and the click handler
  (:637); apply the same window to the DSP panels. Live mode keeps its 30 s sliding window.

### 6. CSV export (separate change)
- Button per DSP panel; `Blob` + `<a download>` like the PNG export; string built in chunks.
- Rows = time, columns = subcarriers, first column `time_s`; masked cells empty;
  `#` header lines with provenance (`interp`, `norm`, `max_gap_ms`, `target_fs`, source).
- Respects "Hide null subcarriers" and the visible time window.

## Tests

- `dsp`: `normalize` `PerSample` ignores null columns (compare against hand-computed μ/σ on
  non-null columns; null columns = 0); identical output when `null_bins` is empty.
- `dsp`: `resample_with` — isolated gap, long gap with and without threshold, and spline
  independence across a masked gap (values in one segment unchanged by data in another).
- `dsp`: amplitude from `from_packets` equals `packet.amplitudes()`; phase equals `atan2(Q, I)`
  on `capture_v8.bin` (locks the Q-first convention for the DSP path too).
- `tests/visualization.rs`: `dsp_view_is_optional` (modelled on `diagnostic_trace_is_optional`,
  :281 — base payload identical with and without DSP); `dsp_grid_matches_slot_grid_on_capture`
  (column `j` ↔ diagnostic `(second, slot)`); null bins in `capture_v8.bin` are exactly
  `{0,1,2,3,32,61,62,63}`.
- Streaming: payload carries `dsp: null` and `StreamingConfig` is unchanged.
- Web (manual checklist, no JS test harness in the repo): toggle visible only with `dsp`;
  live shows the note; hide-null removes rows; phase colorbar spans ±π; CSV loads in
  `pandas.read_csv(comment='#')`.
- Note: tests that read `capture_v8.bin` need it copied to the repo root first (`Memory.md` §1).

## Docs

- `docs_EN/visualization.md`: new "DSP view" feature section after the diagnostic view
  (:57-171); payload fields; CLI options table (:188). Mirror in `docs_VN`.
- `docs_EN/dsp.md`: `CsiSignal.null_bins` (:24), null-bin rule under `normalize` (:85),
  `resample_with` and the gap threshold under Resampling (:109). Mirror in `docs_VN`.
- `docs_EN/streaming.md`: one line — live mode has no DSP view. Mirror in `docs_VN`.
- `Memory.md`: header line 8 (points at this plan), §2.6 DSP-view entry, and replace the
  "Planned application: a DSP panel" bullet (:189) with decision D1.

## Implementation order

1. **M1 — Snapshot DSP** (next change): `dsp` changes (§1) + tests → `visualization.rs` (§2) →
   CLI (§3) → web panels and live note (§4) → docs EN → VN → `Memory.md`.
2. **M2 — Time-window selector** (§5).
3. **M3 — CSV export** (§6).
4. **M4 — More transforms** in the panel: FFT/STFT, filters; phase unwrap / per-frame detrend.
   Panels are already self-describing (`id`, `unit`, `kind`), so STFT's different axes only
   need an axis description added to `DspPanel`.
5. **M5 — WASM** (D3): build `dsp` for `wasm32`, embed, interactive selectors in the shared HTML.

## Open questions

1. Offline default: DSP **on** (per the offline-exempt rule) with `--no-dsp` to opt out, or opt-in with `--dsp`?
2. Null-bin source: the HT20 constant for 64-bin captures (assumed), or detect all-zero columns so other layouts work too?
3. Selecting `--dsp-norm`: add the normalized panel next to the resampled ones (assumed) or replace them?
4. Null rows default: masked-but-visible (assumed) or hidden?
5. 5-minute limit: maximum **visible window** (assumed) or a cap on recording length?
6. Window selector scope: one shared slider or one per canvas; does the diagnostic slot grid stay per-second?
7. CSV: visible window (assumed) or the full recording by default?