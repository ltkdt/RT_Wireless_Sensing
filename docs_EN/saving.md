# Saving module

Source: [`src/saving.rs`](../src/saving.rs) · Standalone binary:
[`src/bin/save.rs`](../src/bin/save.rs)

This module converts a collected binary capture into numeric arrays for Python
or Rust analysis. It decodes the capture with the [decoder](decoder.md) and
writes one `.npz` archive next to the input, keeping the original file stem.

```text
capture_v8.bin  ──decode──▶  capture_v8.npz
                             ├─ timestamp_ms   (N,)
                             ├─ amplitude_00…63 (N,)
                             └─ phase_00…63     (N,)
```

## Output layout

`N` is the number of decoded records. The standard ESPectre HT20 capture has 64
complex subcarriers, producing **1 + 64 + 64 = 129 arrays**:

| Array name | Shape | dtype | Contents |
|---|---|---|---|
| `timestamp_ms` | `(N,)` | `float64` | Sample time in milliseconds, first record = `0.0` |
| `amplitude_00` … `amplitude_63` | `(N,)` | `float32` | `hypot(I, Q)` for that subcarrier |
| `phase_00` … `phase_63` | `(N,)` | `float32` | `atan2(Q, I)` for that subcarrier, radians |

- Subcarrier indices are zero-based and match the decoder's payload order
  (centered HT20: bin 0 is the first guard/null bin).
- Amplitude and phase are derived from the decoder's `(I, Q)` convention:
  `payload[0] = Q`, `payload[1] = I`.
- Phase is the raw wrapped value in `[-π, π]`; it is **not** unwrapped or
  smoothed.
- Null subcarriers are preserved as zero amplitude / zero phase; they are not
  removed.
- If a capture uses a different consistent `num_sc`, the archive has
  `1 + 2 * num_sc` arrays and the index names are still zero-padded to two
  digits. A capture whose `num_sc` changes mid-stream is rejected.

## Timestamps

The raw wire format has no wall-clock field. Every sample carries a monotonic
64-bit `device_ticks_us`, so timestamps are computed relative to the first
record:

```text
timestamp_ms[i] = (device_ticks_us[i] - device_ticks_us[0]) / 1000
```

Consequences:

- The first value is exactly `0.0`.
- The unit is milliseconds (`f64`); at 20 PPS the spacing is about 50 ms.
- Times are sample times from the device, not HTTP arrival times, so network
  jitter and decode time do not affect them.
- `device_ticks_us` must be non-zero and non-decreasing. A capture that violates
  this fails with an error rather than writing misleading timestamps.
- The original absolute tick values are not stored; only the relative time is.

`wifi_rx_ts_us` is deliberately not used: it is only present when its validity
flag is set, is 32-bit (it wraps after about 71 minutes), and on some chips uses
a different clock than `device_ticks_us`.

## Important API

### `save_capture`

```rust
pub fn save_capture(bin_path: &Path) -> Result<SaveSummary, SaveError>
```

Decodes `bin_path` and writes `npz_path_for(bin_path)`.

```rust
use std::path::Path;
use rt_wireless_sensing::saving::save_capture;

let summary = save_capture(Path::new("capture_v8.bin"))?;
println!("{} records -> {}", summary.records, summary.output.display());
```

### `save_capture_to`

```rust
pub fn save_capture_to(
    bin_path: &Path,
    npz_path: &Path,
) -> Result<SaveSummary, SaveError>
```

Same as `save_capture`, but the caller chooses the archive path. Useful for
batch jobs or tests.

Arguments:

- `bin_path` — raw capture produced by [`collection`](collection.md), for
  example `capture_v8.bin`.
- `npz_path` — destination archive. The parent directory must exist.

Output: [`SaveSummary`](#savesummary), or [`SaveError`](#saveerror).

### `npz_path_for`

```rust
pub fn npz_path_for(bin_path: &Path) -> PathBuf
```

Replaces the extension with `.npz`: `capture_v8.bin` → `capture_v8.npz`,
`data/run.bin` → `data/run.npz`. A path with no extension gets `.npz` appended.

### `SaveSummary`

| Field | Type | Meaning |
|---|---|---|
| `input` | `PathBuf` | Source `.bin` |
| `output` | `PathBuf` | Written `.npz` |
| `records` | `usize` | Decoded records (`N`) |
| `subcarriers` | `usize` | Complex subcarriers per record |
| `arrays` | `usize` | `1 + 2 * subcarriers` |
| `duration_ms` | `f64` | Last timestamp in ms |
| `pps` | `f64` | Effective rate from device ticks |
| `bytes_written` | `u64` | Size of the `.npz` file |

### `SaveError`

| Variant | Cause |
|---|---|
| `Io(std::io::Error)` | Input read or archive write failed |
| `Decode(DecodeError)` | Malformed or truncated capture |
| `NoRecords` | No complete CSI record in the file |
| `NoSubcarriers { record }` | A record reports zero subcarriers |
| `MissingDeviceTicks { record }` | `device_ticks_us == 0`, so no relative time exists |
| `NonMonotonicDeviceTicks { record, previous, found }` | Device clock moved backwards |
| `SubcarrierCountChanged { record, expected, found }` | Layout changed mid-capture |
| `WriteNpz(WriteNpzError)` | ZIP/NPY writer failure |

## Standalone module

```bash
# capture_v8.bin -> capture_v8.npz
cargo run --bin save -- capture_v8.bin

# Choose the archive path
cargo run --bin save -- capture_v8.bin --output arrays.npz

# Help
cargo run --bin save -- --help
```

The default input is `capture_v8.bin`. Expected output for the repository
capture:

```text
input        : capture_v8.bin
output       : capture_v8.npz (389274 bytes)
records      : 691
subcarriers  : 64
arrays       : 129
duration     : 34499.075 ms
rate         : 20.00 records/s
```

## Reading the result

Python:

```python
import numpy as np

with np.load("capture_v8.npz") as data:
    print(len(data.files))          # 129
    t = data["timestamp_ms"]        # (691,) float64, starts at 0.0
    amp = data["amplitude_04"]      # (691,) float32, first live subcarrier
    phase = data["phase_04"]
    matrix = np.stack([data[f"amplitude_{k:02d}"] for k in range(64)], axis=1)
```

Rust:

```rust
use ndarray::Array1;
use ndarray_npy::NpzReader;
use std::fs::File;

let mut npz = NpzReader::new(File::open("capture_v8.npz")?)?;
let timestamps: Array1<f64> = npz.by_name("timestamp_ms")?;
let amplitude: Array1<f32> = npz.by_name("amplitude_04")?;
```

## Notes

- The writer uses `ndarray-npy`, already a dependency of this crate.
- A malformed archive is not silently repaired: `save_capture_to` returns the
  decoder error with its byte offset.
- This module does not perform DSP, filtering, or inference. It only exports the
  decoded timestamp, amplitude, and phase values.
