# CSI decoder module

Source: [`src/decoder.rs`](../src/decoder.rs) · Standalone binary:
[`src/bin/decode.rs`](../src/bin/decode.rs) · Schema:
[`src/csi_header.rs`](../src/csi_header.rs)

This module parses and validates the ESPectre V8 CSI stream. It handles frames
split across reads, several frames per read, and reports corruption with an
absolute byte offset instead of silently resynchronizing.

## Wire format

One frame is:

```text
┌──────────────────────────┬────────────────────────┬──────────────────┐
│ HTTP prefix (60 B)       │ CSI record header (64 B)│ CSI I/Q payload  │
│ RawCsiHttpFramePrefix    │ RawCsiRecordHeaderV8    │ csi_len_bytes    │
└──────────────────────────┴────────────────────────┴──────────────────┘
```

`frame_len = prefix.header_len + prefix.record_len`, and `record_len` covers the
record header plus payload. The decoder checks both layers, their length
relationships, the session id, sequence counters, and the four
prefix↔record counter cross-checks.

## Important API

### `FrameStreamDecoder`

Incremental decoder. Use it for files, sockets, or any chunked input.

| Method | Signature | Meaning |
|---|---|---|
| `new` | `fn new() -> Self` | Empty decoder |
| `with_capacity` | `fn with_capacity(bytes: usize) -> Self` | Preallocate the buffer |
| `push` | `fn push(&mut self, chunk: &[u8])` | Append raw bytes; chunks may split frames anywhere |
| `next_frame` | `fn next_frame(&mut self) -> Result<Option<CsiPacket>, DecodeError>` | Decode one frame: `Some` = packet, `None` = need more bytes, `Err` = invalid stream |
| `finish` | `fn finish(&self) -> Result<(), DecodeError>` | Call after input ends; rejects truncated final frames and trailing bytes |
| `session_id` | `fn session_id(&self) -> Option<[u8; 16]>` | Session learned from the first frame |
| `frames_decoded` | `fn frames_decoded(&self) -> u64` | Successful frames so far |
| `bytes_consumed` | `fn bytes_consumed(&self) -> u64` | Absolute offset of the first undecoded byte |
| `buffered_len` | `fn buffered_len(&self) -> usize` | Bytes held for a partial frame |

File example:

```rust
use std::fs;
use rt_wireless_sensing::decoder::FrameStreamDecoder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read("capture_v8.bin")?;
    let mut decoder = FrameStreamDecoder::new();
    decoder.push(&bytes);

    while let Some(packet) = decoder.next_frame()? {
        println!(
            "seq={} ticks={} amplitude[4]={:.3}",
            packet.frame.stream_sequence,
            packet.csi.device_ticks_us,
            packet.amplitudes()[4],
        );
    }
    decoder.finish()?;
    Ok(())
}
```

Socket-style example:

```rust
let mut decoder = FrameStreamDecoder::new();
while let Some(chunk) = stream.next().await {
    decoder.push(&chunk?);
    while let Some(packet) = decoder.next_frame()? {
        process(packet);
    }
}
decoder.finish()?;
```

### `CsiPacket`

One decoded record:

| Field / method | Type | Meaning |
|---|---|---|
| `frame` | `HttpFrameHeader` | 60-byte transport prefix fields |
| `csi` | `CsiHeader` | 21-field CSI record header |
| `iq_raw` | `Vec<i8>` | Payload in wire order `[Q0, I0, Q1, I1, …]` |
| `subcarriers()` | `usize` | `csi.num_sc` |
| `iq()` | `&[i8]` | Raw interleaved payload |
| `iq_pairs()` | `impl Iterator<Item = (f32, f32)>` | `(I, Q)` pairs, wire order converted |
| `amplitudes()` | `Vec<f32>` | `hypot(I, Q)` per subcarrier |
| `phases()` | `Vec<f32>` | `atan2(Q, I)` per subcarrier, radians |
| `valid_wifi_rx_ts_us()` | `Option<u32>` | Driver RX timestamp if its flag is set |
| `valid_wifi_rx_start_ts_ns()` | `Option<u64>` | RX-start timestamp if its flag is set |
| `valid_noise_floor_dbm()` | `Option<i8>` | Noise floor, `None` for the `-128` sentinel |

`CsiHeader` (`packet.csi`) exposes `seq_num`, `num_sc`, `csi_len_bytes`,
`device_id`, `device_ticks_us`, `wifi_rx_ts_us`, `channel`, `rssi_dbm`,
`noise_floor_dbm`, `flags`, plus name helpers (`chip_name()`,
`phy_mode_name()`, `ltf_type_name()`, `channel_width_name()`).

### `HttpFrameHeader::parse`

```rust
pub fn parse(bytes: &[u8], base: u64) -> Result<HttpFrameHeader, DecodeError>
```

Parses only the 60-byte prefix from `bytes`; `base` is the absolute file/stream
offset used in error messages. Length, session, and counter validation is done
by `FrameStreamDecoder::next_frame`, because it needs per-stream state.

### `DecodeError`

Twenty variants, each carrying an absolute `offset` (for example `Truncated`,
`BadFrameMagic`, `SessionChanged`, `StreamSequenceNotIncreasing`,
`RecordLengthMismatch`, `CounterMismatch`, `TrailingBytes`). It implements
`Display` and `std::error::Error`.

## Standalone module

```bash
# Text table plus summary
cargo run --bin decode -- capture_v8.bin

# Only the first 10 rows, then the summary
cargo run --bin decode -- capture_v8.bin --limit 10

# JSON Lines (one object per record, final summary object)
cargo run --bin decode -- capture_v8.bin --json

# Summary only
cargo run --bin decode -- capture_v8.bin --quiet
```

The default input is `capture_v8.bin`. The summary reports frame count, bytes,
payload FNV-1a 64 checksum, session/device ids, sequence gaps, drop and
backpressure deltas, duration, and records per second. Raw I/Q arrays are not
printed; use [`saving`](saving.md) for numeric export.

Validation reference: `capture_v8.bin` must decode to 691 records with payload
hash `0x6446fc78e2c7f3e6` and zero trailing bytes.
