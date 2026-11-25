# CSI visualization module

Source: [`src/visualization.rs`](../src/visualization.rs) · Standalone binary:
[`src/bin/visualize.rs`](../src/bin/visualize.rs) · Web assets:
[`web/`](../web)

This module provides a scientific graphical dashboard for visualizing decoded Wi-Fi CSI data. It retrieves CSI records from the [decoder](decoder.md) and displays them as a 2D amplitude heatmap using the HTML Canvas 2D API, matching the requirements in `plan.md`.

```text
capture_v8.bin ──decode──▶ VisualizationData ──export──▶ capture_v8_heatmap.html
                             ├─ metadata                  (standalone single-file dashboard)
                             ├─ timestamps (s)       OR
                             ├─ amplitudes [N, 64]   ──serve───▶ http://127.0.0.1:8080/
                             └─ classification (opt)
```

## Features

### 1. 2D CSI Amplitude Heatmap
* **Renderer**: Native HTML5 Canvas 2D API with high-DPI (Retina) support.
* **X-axis**: Measurement time in seconds. Static captures start at `0.0`; the
  live streaming window keeps absolute labels (for example, `2–32 s` once the
  30 s window has slid), preserving temporal ordering.
* **Y-axis**: Subcarrier index ($0 \dots 63$). Subcarrier 0 is rendered at the bottom and subcarrier 63 at the top.
* **Colormaps**: Perceptually uniform scientific colormaps (**Viridis** [default], **Plasma**, and **Jet**).
* **Colorbar**: Vertical gradient with numeric amplitude ticks and units (`Amplitude (a.u.)`).
* **Interactivity**: Dynamic mouse hovering with crosshair guidelines, coordinate readout (Time, Subcarrier, Amplitude, Frame), and tooltip popup.
* **Occupancy on click**: Click a heatmap point to compute the occupancy of that point's second. See [Occupancy](#4-click-to-compute-occupancy).

### 2. Measurement Information
Displays key capture statistics alongside the heatmap:
* **Start Time**: Relative start and monotonic device clock (`device_ticks_us`).
* **Duration**: Total elapsed capture time in seconds.
* **Decoded Frames**: Total valid CSI records ($N$).
* **Packet Rate**: Two values side by side.
  * **Nominal** — the requested `--pps` argument (`metadata.nominal_pps`). When
    no `--pps` was supplied (`nominal_pps` is `null`), the card shows the default
    nominal rate (20 PPS, `collection::DEFAULT_PPS` / `DEFAULT_NOMINAL_PPS` in
    the web layer) in a muted style and says so in the caption; it never
    presents the default as a requested value.
  * **Effective** — the measured rate, `(frames − 1) / duration`, computed from
    device clock deltas (`metadata.effective_pps`). It is always measured, never
    a fallback for the nominal value.

### 3. Classification Results Area
Provides a dedicated card displaying predicted activity or gesture classes (e.g. "Standby / None", "Walking", "Waving") with confidence levels when classification results are connected.

### 4. Click-to-compute occupancy
Clicking a heatmap point selects the second containing that point (for example,
`24.3 s → [24, 25)`). The visualizer divides that second into `N` equal slots,
where `N` is the nominal PPS, and reports:

```text
occupancy = (number of slots containing at least one measurement) / N
```

The result is shown in the hover row as **Occupancy (click)**, e.g.
`87.0% (87/100 @ 24s)`. `N` is the *nominal* rate: the streaming command passes
`--pps` (`metadata.nominal_pps`), and a capture decoded without `--pps` falls
back to the default nominal rate (20 PPS) — not to the measured rate, which is
reported separately as effective PPS. The value is recomputed on each live
data refresh and disappears when the second leaves the 30 s window.

This is a browser-side calculation over the displayed `timestamps`; no extra
endpoint is involved.

### 5. Diagnostic view (PPS-deficit analysis)

A separate canvas behind a bottom toggle, independent of the amplitude heatmap
(which is untouched while the toggle is off). It answers one question: *why does
each second yield fewer CSI samples than the configured PPS?*

Click a heatmap point to select its second, then enable **Diagnostic view**. The
panel opens with a **Capture overview** covering the whole trace, followed by the
**Slot grid — selected second**. The grid draws the `subcarriers × slots` matrix
of that second (`slots` = nominal PPS rounded once in Rust); empty columns are
hatched. Hovering a cell shows a one-line identity
(`Slot 12/99 · SC 31 — filled (frame #431)`); clicking a cell fills the
**Timeslot details** box below the canvas, and the **Statistics** box shows the
per-second numbers.

Every value is computed in Rust from the `.bin` capture itself — the 60-byte
HTTP frame prefix and the 64-byte CSI record header — and shipped in the
`diagnostics` payload field, so the same numbers can be exported with `--json`
for offline analysis.

#### Capture overview

The overview aggregates every empty slot of the trace, so it answers *where the
PPS went* for the whole capture instead of one second at a time. It renders as
soon as the toggle is on, with or without a selected second, and refreshes on
every poll.

- **KPI row:** records / total slots, loss %, empty slots, dropped frames,
  Δ`raw_drop_total`, Δ`raw_send_backpressure_total`.
- **Donut:** the empty slots split by cause, one fixed colour per cause, with a
  legend row per cause showing the count and its percentage of the empty slots.
  `ambiguous` is its own slice, so a live-window edge never distorts the
  attribution of the other causes.
- **Tooltips:** hovering a KPI, a legend row or the ⓘ glyph explains the
  underlying payload field.

Percentages are over the empty slots inside the **span of the run**, which is the
declared span when `--duration` was given and the recorded span otherwise. The
span is rounded **once** into an integer number of slots
(`round(span_seconds × slots_per_second)`), so `5.006 s` at 100 PPS is 501 slots
and `--duration 5` is 500. `filled_slots` counts the slots that received at least
one record and `empty_slots` the rest, so the two always add up to `total_slots`.

Records that arrived after the span (a deadline stops the reader, not the writer)
are still in `records[]` but have no slot of their own: they are not coverage, so
they are not part of `filled_slots` and are reported once as
`records_past_span`. `summary.extra_records = filled_records − filled_slots`
counts the different case — records that landed in a slot another record already
filled. Edges are marked by `capture_end` and `seconds[].partial`.

#### Payload

| Field | Meaning |
|---|---|
| `slots_per_second` | Column count of the grid (nominal PPS rounded once) |
| `origin_ticks` | `device_ticks_us` that maps to timestamp `0.0` |
| `capture_end` | `true` for an offline file, `false` for a live window edge |
| `records[]` | One row per received record: slot, slot offset, Δstream, Δdrop, Δbackpressure, Δticks, Δwifi_rx, RSSI/noise/SNR, flags, channel/PHY/width |
| `empty_slots[]` | One row per slot without a record: cause, dropped frames, boundary records |
| `seconds[]` | Per-second statistics: counts, Δtick intervals, bursts, slot offsets, layer counters, radio quality, anomalies |
| `records_past_span` | Records that arrived after the declared span; they are in `records[]` but have no slot of their own, so they are not coverage |
| `summary` | Capture-level aggregation for the overview panel: `total_slots` (`round(span × slots_per_second)`, from `--duration` when given), `filled_slots`, `filled_records`, `extra_records`, `empty_slots`, `loss_ratio`, `cause_counts`, `dropped_frames_total`, `delta_drop_total`, `delta_backpressure_total`, `empty_slots_low_rssi` |

`summary` is `#[serde(default)]`: a payload written before the field existed
still parses, and the web layer then aggregates `empty_slots` itself.

The whole `diagnostics` field is optional. Offline payloads (`visualize`,
`VisualizationData::from_file*`, `prepare_visualization_data*`) always carry it;
the live dashboard only builds it when started with `--diagnostics`, because it
costs about 1 MB per poll. When absent, the payload carries
`"diagnostics": null` and the web layer hides the toggle — the amplitude heatmap
and the browser-side occupancy readout are unaffected.

#### Slot semantics

`slot = floor((device_ticks_us − second_start) × slots / 1_000_000)`, computed
with integer arithmetic on device ticks — the exact form of the browser
occupancy formula `floor((t − second_start) × slots + 1e-6)`. A record belongs
to the half-open slot containing its `device_ticks_us`; `slot_offset_us` is the
signed distance from the theoretical slot start (positive = late).

#### Which layer is at fault?

All counters are recorded on the RX device, but each one points at a different
layer:

| Signature | Layer | Conclusion | Next action |
|---|---|---|---|
| Empty slot, `stream_sequence` gap, Δ`raw_drop_total` > 0 | **RX (device)** — record not sent (queue full / invalid / batch fail) | Device drop due to full queue | Lower PPS, increase batch size, check firmware CPU |
| Empty slot, gap, Δ`raw_send_backpressure_total` > 0 | **Desktop** — client did not accept the batch | Desktop reads too slowly | Increase client buffer, slow the 500 ms refresh |
| Empty slot, contiguous counters, low neighbouring RSSI | **TX/airtime** (AP → device) | Over-air loss (airtime) | Change channel, reduce other traffic, lower PPS |
| Empty slot, contiguous counters, normal RSSI | **Generator (desktop)** | Marker not sent / firmware produced no CSI | Corroborate with the sent-marker log |
| Sample present, large slot offset, high interval std | **Generator (desktop)** jitter | Timing jitter | Measure the generator |

`raw_drop_total` is an **RX-side** drop (the record never left the device);
`raw_send_backpressure_total` is a **desktop-side** receive failure. Same symptom
(record missing from the file), opposite sides.

"Low RSSI" means at or below `LOW_RSSI_THRESHOLD_DBM` (−70 dBm) on either
boundary record. A gap the counters do not explain is reported as `ambiguous`
rather than guessed.

#### Limitations

- No marker identity: the capture does not record which UDP marker produced a
  record, so "marker not sent" cannot be proven from the file alone.
- The link is HTTP over TCP, so in-transit loss is impossible: a
  `stream_sequence` gap is a device-side drop, not a network drop.
- `wifi_rx_ts_us` is a different clock domain (and 32-bit), so only its relative
  Δ is meaningful; `seq_num` saturates at `u32::MAX`.
- The last second of a capture, and both edges of a live window, are partial:
  `seconds[].partial` is set and the loss ratio describes only the covered part.
- The payload is not free: a 34 s capture at 100 slots carries about 1 MB of
  diagnostics (records + empty slots + statistics) next to the amplitude matrix.

#### `--pps` for offline files

Without `--pps`, `metadata.nominal_pps` is `null` and the slot grid falls back
to the default nominal rate (20 PPS), so a capture recorded faster than 20 PPS
reports more than one measurement per slot. Pass the rate the capture was
recorded at to align the grid, the occupancy readout, and the diagnostic slots
with the intended size:

```bash
cargo run --bin visualize -- capture_v8.bin --pps 100
```

### 6. DSP view (resampling and normalization)

An optional second panel set, computed **once in Rust** from the same decoded
packets as the heatmap and embedded in the payload. It is available only for
offline data: while a dashboard is streaming, `dsp` is `null` and the toggle is
replaced by the note *"DSP analysis is available after streaming is closed."*

The browser never resamples or normalizes. It only formats what Rust computed,
so there is exactly one implementation of every formula.

#### Panels

| Panel | `kind` | Unit | What it is |
|---|---|---|---|
| `amplitude` | `sequential` | `a.u.` | Resampled $\lvert h_k(t)\rvert$ on the uniform grid |
| `phase` | `cyclic` | `rad` | Resampled $\arg h_k(t)$, colour-mapped over $-\pi\ldots\pi$ |
| `amplitude_norm` | `diverging` | `z` or `0–1` | Normalized amplitude; present only when a scheme was chosen |

Phase is `atan2(Q, I)` on the interpolated I/Q values, never interpolated
directly: raw phase wraps at $\pm\pi$, so interpolating it would create
discontinuities that do not exist in the signal.

The amplitude panel clips its colour scale to the 2nd–98th percentile and
labels the full extent in the panel meta. A single multipath spike (this capture
reaches 29 252 against a median of 26) would otherwise pin the scale and
collapse 99% of the data into one colour.

#### Grid

The grid rate is `slots_per_second` (the nominal PPS rounded once in Rust), so
resampled column `j` maps to diagnostic slot `(second = j / N, slot = j % N)`.
The diagnostic slot grid and the DSP panels therefore line up exactly.

#### Masking

The mask is **separable**, so no per-cell matrix is shipped:

- **Null subcarriers** (`null_subcarriers`): the eight guard/DC bins of a
  centred HT20 capture, `{0,1,2,3,32,61,62,63}`. They never carry channel
  information, are never rendered as a real `0`, and are excluded from every
  transform across the subcarrier axis (`normalize` with `PerSample` computes
  its mean and deviation over the non-null columns only). The panel has a
  **Hide null subcarriers** option; y-axis ticks keep physical bin indices.
- **Time columns** (`time_state`): `0` observed, `1` bridged, `2` masked.
  A column is *bridged* when it was interpolated across a gap longer than 1.5
  nominal intervals, and *masked* when that gap exceeds `max_gap_ms`. Masked
  cells are withheld — rendered hatched and exported as empty — not filled with
  an invented value.

When a gap threshold is set, the source series is split at every over-threshold
gap and each segment is interpolated independently. A natural cubic spline is a
global fit, so interpolating across a dropout and masking afterwards would let
that dropout distort its neighbours.

#### Time window

Every DSP canvas shares one time-window selector (default 30 s, maximum 5
minutes). It is a **canvas restriction only**: the recording and the CSV export
always keep every grid column.

#### CSV export

Each panel has an **Export CSV** button. The file covers the **full recording**,
not the visible window: one row per grid column, one column per subcarrier,
preceded by `#` provenance lines (`interp`, `target_fs`, `norm`, `max_gap_ms`,
source) so it loads directly with `pandas.read_csv(path, comment='#')`. Masked
cells are left empty, and the `time_state` column says why.

#### Payload

```json
"dsp": {
  "interp": "cubic_spline",
  "target_fs": 100.0,
  "norm": null,
  "max_gap_ms": null,
  "timestamps": [0.0, 0.01, "…"],
  "null_subcarriers": [0, 1, 2, 3, 32, 61, 62, 63],
  "time_state": [0, 0, 1, "…"],
  "panels": [
    { "id": "amplitude", "title": "Amplitude — resampled", "unit": "a.u.", "kind": "sequential", "data": [["…"]] },
    { "id": "phase", "title": "Phase — resampled", "unit": "rad", "kind": "cyclic", "data": [["…"]] }
  ]
}
```

`dsp` is `#[serde(default)]`, so payloads written before this feature parse
with `null` and the web UI simply hides the toggle.

## Standalone CLI

```bash
# Decode capture_v8.bin and export standalone HTML dashboard
cargo run --bin visualize -- capture_v8.bin

# Specify custom HTML output path and open automatically in default browser
cargo run --bin visualize -- capture_v8.bin --output dashboard.html --open

# Export JSON visualization data
cargo run --bin visualize -- capture_v8.bin --json csi_data.json

# Launch embedded asynchronous web server
cargo run --bin visualize -- capture_v8.bin --serve --port 8080

# Count slots over the 5 s that were requested, not over the 5.006 s recorded
cargo run --bin visualize -- capture_v8_internal.bin --pps 100 --duration 5
```

### CLI options

| Option | Description |
|---|---|
| `FILE` | Raw capture file written by `collect` (default: `capture_v8.bin`) |
| `-o, --output FILE` | Destination path for standalone HTML dashboard (default: `<stem>_heatmap.html`) |
| `--json FILE` | Path to export JSON visualization data |
| `--pps PPS` | Nominal packet rate, shown as **Nominal** in the UI, used for the diagnostic/occupancy slot grids and the DSP grid. Without it `metadata.nominal_pps` is `null`, the UI shows the default nominal rate (20 PPS), and the DSP grid uses the measured rate |
| `--duration DUR` | Span of the run that was asked for, in `5`, `5s`, `500ms` or `1m` form. It sets `total_slots = round(span × slots/s)`, so `--duration 5 --pps 100` reports 500 slots even when the recorder overshot its deadline. Without it the span is the recorded one. The capture file is never truncated: records past the span stay in the payload and are counted as `records_past_span` |
| `--no-dsp` | Skip the precomputed DSP view to keep the payload small; the DSP toggle stays hidden |
| `--dsp-interp M` | Resampling interpolation: `nearest`, `linear` or `cubic` (default: `cubic`) |
| `--dsp-norm S` | Normalization applied on top of the resampling: `none`, `per-subcarrier`, `per-sample` or `min-max` (default: `none`) |
| `--dsp-max-gap-ms MS` | Mask grid columns whose source gap exceeds `MS` milliseconds (default: no threshold, every gap bridged) |
| `--serve` | Start embedded asynchronous web server |
| `--port PORT` | Port for the web server (default: `8080`) |
| `--open` | Open generated HTML file or web server URL in default system browser |
| `-q, --quiet` | Suppress summary text output |
| `-h, --help` | Show usage information |

## Library API

### Preparing visualization data

```rust
use std::path::Path;
use rt_wireless_sensing::visualization::VisualizationData;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Decode and prepare data from binary file
    let data = VisualizationData::from_file(Path::new("capture_v8.bin"))?;

    println!("Decoded {} frames across {} seconds", 
        data.metadata.total_frames, 
        data.metadata.duration_seconds
    );

    // Export standalone HTML bundle (inlines HTML, CSS, JS, and JSON data)
    data.export_html(Path::new("csi_dashboard.html"))?;

    Ok(())
}
```

Pass the configured packet rate when the capture is known to miss samples, so
the diagnostic slot grid keeps its intended size:

```rust
use rt_wireless_sensing::visualization::{
    VisualizationData, VisualizationOptions, prepare_visualization_data_with_options,
};

// Offline file with an explicit nominal PPS.
let data = VisualizationData::from_file_with_pps(Path::new("capture_v8.bin"), Some(100.0))?;

// Or from decoded packets, with full control over the trace.
let packets = rt_wireless_sensing::visualization::decode_packets(&bytes)?;
let data = prepare_visualization_data_with_options(
    &packets,
    Some("capture_v8.bin"),
    packets[0].csi.device_ticks_us,
    VisualizationOptions { nominal_pps: Some(100.0), capture_end: true },
)?;
```

### Attaching classification results

```rust
let data = VisualizationData::from_file(Path::new("capture_v8.bin"))?
    .with_classification(
        Some("WaveHand".to_string()),
        Some(0.95),
        Some("Model v1.0 inference".to_string()),
    );

data.export_html(Path::new("predicted.html"))?;
```

### Running the embedded web server

```rust
use rt_wireless_sensing::visualization::{VisualizationData, serve_dashboard};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = VisualizationData::from_file(std::path::Path::new("capture_v8.bin"))?;
    serve_dashboard(data, "127.0.0.1:8080").await?;
    Ok(())
}
```

### Running a live dashboard

`serve_dashboard` serves one fixed dataset. For live updates, use
`serve_dashboard_live` (or `serve_dashboard_live_on` with an already-bound
`TcpListener`) and update a shared [`LiveDashboardSnapshot`]:

```rust
use std::sync::{Arc, RwLock};
use rt_wireless_sensing::visualization::{
    LiveDashboardSnapshot, serve_dashboard_live,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(RwLock::new(LiveDashboardSnapshot::Waiting {
        elapsed_seconds: 0.0,
        warmup_seconds: 3.0,
    }));

    // A producer can replace the snapshot every 500 ms:
    // *state.write().unwrap() = LiveDashboardSnapshot::Ready {
    //     data: Box::new(data),
    // };

    serve_dashboard_live(Arc::clone(&state), "127.0.0.1:8080").await?;
    Ok(())
}
```

`LiveDashboardSnapshot` has two variants:

- `Waiting { elapsed_seconds, warmup_seconds }` — the browser keeps its waiting
  overlay and does not draw a heatmap.
- `Ready { data: Box<VisualizationData> }` — the browser replaces the heatmap.

The [`streaming`](streaming.md) module uses this API to publish a 30 s sliding
window every 500 ms. Its browser code polls `/api/data` every 500 ms and also
accepts the static `VisualizationData` shape emitted by `serve_dashboard`.

## Web Server Endpoints

When running `serve_dashboard`, `serve_dashboard_live`, or `visualize --serve`:
* `GET /` — Serves the dashboard HTML page.
* `GET /api/data` — Returns the current visualization payload as JSON.
* `GET /style.css` — Serves the stylesheet.
* `GET /visualizer.js` — Serves the Canvas 2D visualization script.

The static server returns [`VisualizationData`](#visualizationdata) directly.
The live server returns a tagged snapshot:

```json
{"status":"waiting","elapsed_seconds":1.5,"warmup_seconds":3.0}
{"status":"ready","data":{"metadata":{...},"timestamps":[...],"amplitudes":[[...]],"classification":null}}
```

Both servers send `Cache-Control: no-store` for `/api/data`.

## Real-time streaming

To collect and visualize at the same time, use the
[`streaming`](streaming.md) command rather than running `visualize --serve` on
a file that is still being written:

```bash
cargo run --bin streaming -- --ip 10.178.22.9 --output capture_v8.bin --pps 100 --port 8080
```

Metadata field `nominal_pps` is the slot count used by the occupancy readout.
The streaming module sets it to the configured `--pps`; a capture decoded
without `--pps` leaves it `null` and the UI shows the default nominal rate
(20 PPS) next to the measured `effective_pps`.
