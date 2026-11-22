# Streaming module

Source: [`src/streaming.rs`](../src/streaming.rs) · Standalone binary:
[`src/bin/streaming.rs`](../src/bin/streaming.rs) · Related:
[collection](collection.md), [decoder](decoder.md), [visualization](visualization.md)

This module records CSI and serves a live amplitude heatmap at the same time.
It composes the collection, decoder, and visualization modules; it does not
duplicate their network, parsing, or rendering code.

```text
ESPectre /csi ──▶ collection ──▶ raw .bin
                       │
                       └──chunks──▶ decoder ──▶ 30 s packet window
                                                   │ every 500 ms
                                                   ▼
                                         live dashboard :8080
```

## Command

```bash
cargo run --bin streaming -- \
  --ip 10.178.22.9 \
  --output capture_v8.bin \
  --pps 100 \
  --port 8080
```

Open `http://127.0.0.1:8080` in a browser. The server starts before collection,
so the page is available immediately; the raw capture is written to
`--output` at the same time.

### CLI options

| Option | Default | Meaning |
|---|---|---|
| `--ip ADDRESS` | `10.178.22.9` | ESP32 IPv4 address |
| `--api-port PORT` | `62587` | ESPectre HTTP API port |
| `--marker-port PORT` | `5555` | UDP marker port |
| `--pps N` | `20` | Marker rate; also the occupancy slot count |
| `--origin URL` | `https://test.espectre.dev` | HTTP `Origin` header |
| `--output FILE` | `capture_v8.bin` | Raw capture file |
| `--port PORT` | `8080` | Dashboard port on `127.0.0.1` |
| `--verbose` | off | Print per-chunk collection progress |
| `--diagnostics` | off | Attach the diagnostic trace to every refresh (~1 MB per poll at 100 slots) |
| `-h, --help` | — | Show usage |

The diagnostic toggle in the web UI needs `--diagnostics` in live mode: without
it the payload carries `"diagnostics": null` and the toggle is hidden. Occupancy
(click-to-compute) is unaffected either way, because it is derived in the browser
from the timestamps.

## Timing behavior

- **0–3 s:** the dashboard shows only a waiting message ("Waiting for CSI
  data… x / 3 s"). No heatmap is drawn.
- **3 s onward:** the first heatmap is published. A snapshot is published
  every **500 ms**.
- **Sliding window:** only the most recent **30 s** of device time is kept.
  At 32 s runtime, data from 0–2 s is dropped.
- **Absolute axis:** the x-axis shows elapsed time since the start of the
  capture, so the window at 32 s is labelled `2–32`, not `0–30`.
- The first snapshot is never published before 3 s, even if records arrive
  earlier. If no record has arrived at 3 s, the waiting message stays until the
  first record.

The window is cut by `device_ticks_us` (the device's monotonic sample clock),
not by wall-clock arrival time. A slow or bursty network therefore does not
distort the time axis.

## Occupancy (click-to-compute)

Click anywhere on the heatmap. The command takes the second containing the
clicked point (`24.3 s → [24, 25)`), divides that second into **N equal slots**
where `N` is the configured `--pps`, and counts how many slots contain at least
one measurement:

```text
occupancy = N_filled / N
```

Example: `--pps 100`, 87 slots contain a packet → `87.0% (87/100 @ 24s)`.
The result appears in the hover row as **Occupancy (click)**. It is
recomputed when the live window refreshes and disappears once that second
leaves the 30 s window.

The slot count comes from `metadata.nominal_pps` (set to `--pps` by this
module), which is the requested rate. The metadata card shows it next to
`metadata.effective_pps`, the rate actually measured from the device clock;
the two differ whenever the device cannot keep up with `--pps`.

## Library API

### `StreamingConfig`

```rust
pub struct StreamingConfig {
    pub collection: CollectionConfig,
    pub port: u16,                    // default 8080
    pub warmup: Duration,             // default 3 s
    pub update_interval: Duration,    // default 500 ms
    pub window: Duration,             // default 30 s
    pub diagnostics: bool,            // default false
}
```

- `StreamingConfig::default()` uses the standard ESP32 address, 20 PPS, and
  `capture_v8.bin`. Per-chunk collection logging is off by default.
- `diagnostics` is off by default: every published snapshot then carries
  `"diagnostics": null` instead of a ~1 MB trace. Set it to `true` (or pass
  `--diagnostics`) to enable the diagnostic view in the web UI.
- `config.bind_addr()` returns `127.0.0.1:<port>`.

### `run`

```rust
pub async fn run(
    config: &StreamingConfig,
) -> Result<StreamingSummary, StreamingError>
```

Starts the dashboard, starts collection, decodes live, and runs until Ctrl+C or
the collection stream ends.

```rust
use rt_wireless_sensing::streaming::{StreamingConfig, run};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = StreamingConfig::default();
    config.collection.esp32_ip = "10.178.22.9".to_string();
    config.collection.pps = 100;
    config.port = 8080;

    let summary = run(&config).await?;
    println!(
        "{} records, {} bytes -> {}",
        summary.records_decoded,
        summary.bytes_written,
        summary.output.display()
    );
    Ok(())
}
```

### `StreamingSummary`

| Field | Meaning |
|---|---|
| `output` | Raw `.bin` written during the run |
| `records_decoded` | Total decoded records, including those already outside the window |
| `window_records` | Records in the final published window |
| `bytes_written` | Size of the raw capture when streaming stopped |
| `duration` | Wall-clock duration of the run |

### `StreamingError`

Wraps `CollectionError`, `DecodeError`, `VisualizationError`, I/O errors,
background-task join errors, and invalid configuration. Implements `Display`
and `std::error::Error`.

## Dashboard endpoints

The live server uses the visualization module's assets and endpoints:

- `GET /` — dashboard HTML
- `GET /api/data` — current snapshot
- `GET /style.css` — stylesheet
- `GET /visualizer.js` — JavaScript

`/api/data` is either:

```json
{"status":"waiting","elapsed_seconds":1.5,"warmup_seconds":3.0}
```

or:

```json
{"status":"ready","data":{"metadata":{...},"timestamps":[...],"amplitudes":[[...]],"classification":null}}
```

The browser polls every 500 ms with `Cache-Control: no-store`.

## Notes

- Press **Ctrl+C** to stop collection, the UDP markers, and the server. The raw
  capture written so far remains on disk.
- A partial final frame caused by Ctrl+C is ignored by the live view; the `.bin`
  still contains the bytes that were written.
- This module writes a raw `.bin`, not a `.npz`. Use [`saving`](saving.md) on the
  capture afterwards if numeric arrays are needed.
- Requires a reachable ESP32 running ESPectre, as with `collect`.
- Live mode never computes the **DSP view**. Resampling, gap splitting and
  normalization need the whole record (whole-record interpolation avoids
  boundary kinks), so the DSP view only exists in the offline
  [`visualization`](visualization.md) path — the dashboard payload carries
  `dsp: null` and the DSP panel stays hidden.
- Only `127.0.0.1` is bound; use an SSH tunnel or a reverse proxy to view it
  from another machine.
