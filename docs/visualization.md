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
* **X-axis**: Measurement time in seconds ($0.0 \dots T_{\max}$), preserving temporal ordering.
* **Y-axis**: Subcarrier index ($0 \dots 63$). Subcarrier 0 is rendered at the bottom and subcarrier 63 at the top.
* **Colormaps**: Perceptually uniform scientific colormaps (**Viridis** [default], **Plasma**, and **Jet**).
* **Colorbar**: Vertical gradient with numeric amplitude ticks and units (`Amplitude (a.u.)`).
* **Interactivity**: Dynamic mouse hovering with crosshair guidelines, coordinate readout (Time, Subcarrier, Amplitude, Frame), and tooltip popup.

### 2. Measurement Information
Displays key capture statistics alongside the heatmap:
* **Start Time**: Relative start and monotonic device clock (`device_ticks_us`).
* **Duration**: Total elapsed capture time in seconds.
* **Decoded Frames**: Total valid CSI records ($N$).
* **Effective Rate**: Packets per second (PPS) computed from device clock deltas.

### 3. Classification Results Area
Provides a dedicated card displaying predicted activity or gesture classes (e.g. "Standby / None", "Walking", "Waving") with confidence levels when classification results are connected.

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

## Web Server Endpoints

When running `serve_dashboard` or `visualize --serve`:
* `GET /` — Serves the dashboard HTML page.
* `GET /api/data` — Returns the prepared [`VisualizationData`](#visualizationdata) as JSON.
* `GET /style.css` — Serves the stylesheet.
* `GET /visualizer.js` — Serves the Canvas 2D visualization script.
