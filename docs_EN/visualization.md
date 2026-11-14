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
* **Effective Rate**: Packets per second (PPS) computed from device clock deltas.

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
`87.0% (87/100 @ 24s)`. The streaming command sets `N` from `--pps`; static
captures use the measured effective PPS. The value is recomputed on each live
data refresh and disappears when the second leaves the 30 s window.

This is a browser-side calculation over the displayed `timestamps`; no extra
endpoint is involved.

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
```

### CLI options

| Option | Description |
|---|---|
| `FILE` | Raw capture file written by `collect` (default: `capture_v8.bin`) |
| `-o, --output FILE` | Destination path for standalone HTML dashboard (default: `<stem>_heatmap.html`) |
| `--json FILE` | Path to export JSON visualization data |
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
The streaming module sets it to the configured `--pps`; file-based visualization
defaults it to the measured `effective_pps`.
