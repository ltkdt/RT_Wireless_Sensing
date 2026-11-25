# CSI stream collection module

Source: [`src/collection.rs`](../src/collection.rs) · Standalone binary:
[`src/bin/collect.rs`](../src/bin/collect.rs)

This module talks to an ESP32 running the
[ESPectre](https://github.com/francescopace/espectre) firmware and records the
raw CSI byte stream. It does not parse the bytes; that is the
[decoder](decoder.md)'s job.

## What one collection run does

1. `GET /espectre/v1/capabilities` to confirm the endpoint is reachable.
2. `PATCH /espectre/v1/sensing` with `{"traffic_generator_mode": "external"}`.
3. Send 4-byte UDP traffic markers (`F0 9F 91 BB`) at the configured PPS.
4. Wait 200 ms so the device starts sampling.
5. `GET /espectre/v1/csi` and copy every response chunk verbatim into the
   output file, overwriting it if it already exists.

With `--internal` (`TrafficGenerator::Internal`) the device produces its own
traffic instead: no `PATCH /sensing` is sent, no markers are generated, and the
device's own `csi_target_pps` is read from `GET /capabilities` and becomes the
nominal rate. The default output name then gains an `_internal` suffix
(`capture_v8_internal.bin`), which is how later consumers recover the mode.

The marker task is aborted when streaming ends or an error occurs. The call
returns when the device closes the stream (or the request fails), or earlier
when `max_duration` elapses.

`max_duration` is a **wall-clock deadline on the reader only**: it stops reading
when the deadline passes, and every chunk already received is written whole. A
5 s run at 100 PPS therefore writes about 500 records and usually one or two more
(the chunk in flight when the deadline fired), and the capture is never
truncated to its nominal length. Counting the slots of the run that was *asked
for* is the visualization's job — `visualize --duration 5` — so the capture file
stays a faithful record of what arrived.

## Important API

### `CollectionConfig`

Settings for one run. All fields are public, so struct-update syntax is the
usual way to change one value.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `esp32_ip` | `String` | `"10.178.22.9"` | ESP32 address |
| `api_port` | `u16` | `62587` | ESPectre HTTP API port |
| `marker_port` | `u16` | `5555` | UDP marker port |
| `pps` | `u32` | `20` | Marker packets per second; must be non-zero. In internal mode it is only the *fallback* nominal rate |
| `traffic_generator` | `TrafficGenerator` | `External` | `External` sends the UDP markers; `Internal` lets the device generate traffic and sends nothing |
| `origin` | `String` | `"https://test.espectre.dev"` | HTTP `Origin` header |
| `output` | `PathBuf` | `"capture_v8.bin"` | Raw capture destination |
| `output_is_default` | `bool` | `true` | Whether `output` may gain the `_internal` suffix; every CLI `--output` clears it |
| `startup_delay` | `Duration` | `200 ms` | Wait between sensing-enable and `/csi` |
| `max_duration` | `Option<Duration>` | `None` | Stop recording after this long; `None` records until the stream ends |
| `verbose` | `bool` | `true` | Print progress for every received chunk to stderr |

Two recorded ip of ESP32:
- 10.178.22.9
- 10.178.22.62

Helpers:

- `CollectionConfig::default()` — the values above.
- `CollectionConfig::with_output(path)` — defaults with a different output.
- `config.base_url() -> String` — `http://<ip>:<port>/espectre/v1`.
- `config.marker_period() -> Duration` — `1000 / pps` ms, clamped to at least
  1 ms.
- `config.marker_target() -> Result<SocketAddr, CollectionError>` — the UDP
  destination.

### `collect`

```rust
pub async fn collect(
    config: &CollectionConfig,
) -> Result<CollectionSummary, CollectionError>
```

Runs the full sequence above. It is `async` and must be awaited inside a Tokio
runtime. Dropping the future aborts the HTTP request and the marker task.

Example:

```rust
use rt_wireless_sensing::collection::{CollectionConfig, collect};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = CollectionConfig {
        esp32_ip: "192.168.1.50".to_string(),
        output: "capture.bin".into(),
        ..CollectionConfig::default()
    };

    let summary = collect(&config).await?;
    println!("{} bytes -> {}", summary.bytes_written, summary.output.display());
    Ok(())
}
```

### `CollectionSummary`

Returned by `collect`:

| Field | Type | Meaning |
|---|---|---|
| `output` | `PathBuf` | File that was written |
| `bytes_written` | `u64` | Total body bytes copied |
| `duration` | `Duration` | Wall-clock duration of the streaming phase |
| `configured_pps` | `u32` | `--pps` in force (a fallback rate in internal mode) |
| `traffic_mode` | `TrafficGenerator` | Who produced the traffic this run recorded |
| `device_traffic_mode` | `Option<String>` | `traffic_generator_mode` reported by the device, verbatim |
| `nominal_pps` | `Option<f64>` | Nominal rate the run was planned around |
| `nominal_pps_source` | `Option<NominalPpsSource>` | Where `nominal_pps` came from (`Device` / `Argument` / `Default`) |
| `capabilities` | `String` | Raw JSON from `GET /capabilities` |

### `CollectionError`

| Variant | Cause |
|---|---|
| `Http(reqwest::Error)` | Request failed, non-2xx status, or body read failed |
| `Io(std::io::Error)` | Output file could not be created/written/flushed |
| `InvalidEndpoint { endpoint, source }` | ESP32 address is not a valid socket address |
| `InvalidPps` | `pps == 0` |
| `WrongDeviceMode(Box<device::Mismatch>)` | The device reports the other traffic mode than the one requested |
| `Device(device::DeviceError)` | Any other device failure surfaced while preparing the run |

Implements `Display` and `std::error::Error`; `?` works where
`CollectionError: Into<E>`.

## Standalone module

```bash
# Help and all options
cargo run --bin collect -- --help

# Default device, default output capture_v8.bin
cargo run --bin collect

# Explicit device and output
cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin

# Faster markers, no per-chunk progress
cargo run --bin collect -- --pps 50 --quiet

# Record for 20 seconds (unit defaults to seconds; ms/s/m/h also work)
cargo run --bin collect -- --duration 20

# Let the device generate the traffic instead of sending markers
cargo run --bin collect -- --internal --duration 5
```

Options: `--ip`, `--api-port`, `--marker-port`, `--pps`, `--internal`,
`--origin`, `--output`, `--duration`, `--quiet`, `-h/--help`.

The default binary (`cargo run` or
`cargo run --bin rt-wireless-sensing`) calls `collect` with
`CollectionConfig::default()`, so it needs no arguments.

## Requirements

- The ESP32 must be reachable at `esp32_ip:api_port` and running ESPectre.
- UDP markers are sent to `esp32_ip:marker_port`; the device must be in
  external-traffic mode (the module enables it). `--internal` needs no marker
  path. A run whose requested mode contradicts the device's is refused with
  `CollectionError::WrongDeviceMode` instead of recording unexplained data.
- The output directory must already exist.

The resulting `.bin` is the input to [`saving`](saving.md).
