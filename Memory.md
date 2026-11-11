# Memory — RT-Wireless-Sensing

Living notes for the ESPectre V8 CSI decoder. Update this file as work progresses.

Last updated: 2026-11-11 (data visualization module added; Canvas 2D heatmap implemented)

> **2026-11-11 — visualization update.** The Data Visualization module is now implemented:
> `src/visualization.rs` (data preparation, standalone bundle rendering, Tokio HTTP server),
> `src/bin/visualize.rs` (standalone CLI), `web/` (`index.html`, `style.css`, `visualizer.js`
> Canvas 2D renderer with Viridis/Plasma/Jet colormaps), `docs/visualization.md`, and
> `tests/visualization.rs`. Initial experiment on `capture_v8.bin` successfully decodes
> 691 frames across 64 subcarriers and generates `capture_v8_heatmap.html`.

> **2026-11-11 — modularization update.** The pipeline is now split into
> `src/collection.rs` (live CSI streaming), `src/decoder.rs` (unchanged parser),
> and `src/saving.rs` (`.bin` → `.npz` with 129 arrays). `src/main.rs` is a thin
> wrapper that calls the collector. Each module has a standalone binary
> (`collect`, `decode`, `save`) and a document in `docs/`. Section 2 below
> describes the repository as it was before this change and is kept as history.

---

## 1. Project scope (from `README.md`)

Rust framework for real-time Wi-Fi CSI sensing:

1. ESPectre firmware on an ESP32 captures CSI; this repo does not modify it.
2. This repo opens `/espectre/v1/csi` and writes the raw byte stream to
   `capture_v8.bin` (`src/main.rs`).
3. The captured bytes must be **decoded** into CSI records (the current task).
4. Decoded CSI feeds a web amplitude heatmap, `.npz` storage, and later ONNX
   inference for localization.

Current task: decode `capture_v8.bin`. DSP/AI are explicitly out of scope
(`plan.md` §6).

---

## 2. Current repository state

| File | State | Notes |
|---|---|---|
| `src/main.rs` | working streamer | GETs `/espectre/v1/csi`, writes chunks verbatim; **not yet wired to the decoder** |
| `src/lib.rs` | **new** | exposes `csi_header` and `decoder` as a library |
| `src/csi_header.rs` | **complete schema** | 21 fields, flag/enum constants, name helpers |
| `src/decoder.rs` | **implemented** | LE parser, validation, `FrameStreamDecoder`, 31 unit tests |
| `src/bin/decode.rs` | **new** | CLI: text/JSON report, `--limit`, `--quiet` |
| `tests/capture_v8.rs` | **new** | 6 end-to-end tests over the real 691-frame capture |
| `scripts/verify_decoder.py` | **new** | Rust-vs-Python differential verifier (optional dev tool) |
| `src/dsp.rs` | working helpers | loads `.npy`, amplitude, FFT; unused by `main` |
| `Cargo.toml` | edition 2024 | unchanged; autodetects `lib.rs` and `src/bin/decode.rs` |

Wiring:

- `cargo test` builds the library, the streamer, the decoder binary and the
  integration tests. `decoder.rs` is now compiled and tested.
- The live streamer still only records bytes. Feeding `FrameStreamDecoder` from
  `bytes_stream()` is the next integration step (§9).
- `git status`: `README.md` modified; new `Memory.md`, `src/lib.rs`,
  `src/decoder.rs`, `src/bin/decode.rs`, `tests/capture_v8.rs`.

---

## 3. Sources of truth

Read (not guessed) from the local ESPectre checkout at
`/home/ltkdt/Development/espectre/`:

| Source | What it gives |
|---|---|
| `src/cpp/runtime/raw_csi.h` | authoritative 60-byte `RawCsiHttpFramePrefix` |
| `src/cpp/runtime/csi_raw_record.h` | authoritative 64-byte `RawCsiRecordHeaderV8`, enums, flags |
| `tools/lib/csi_io.py` L98–119 | Python mirror constants + `struct` formats |
| `tools/lib/csi_io.py` L369–456 | `parse_csi_record()` — record validation and I/Q derivation |
| `tools/lib/csi_io.py` L932–990 | `_consume_raw_frames()` — envelope validation and cross-checks |
| `tools/lib/csi_io.py` L343–366 | `_ensure_derived_arrays()` — Q/I convention |
| `src/python/micro_espectre/device_utils.py` L16–41 | HT20 length, null-bin layout constants |
| `docs/API.md` L310–339 | session semantics, counters, drop accounting |
| `docs/CSI.md` L94–120 | normalization to centered 64-SC/128-B layout |
| `test/python/host/cli/test_csi_raw_record.py` L180–260, L660–700 | byte-level fixtures, I/Q convention proof |

All `struct` formats are little-endian with standard sizes and no padding
(`<` prefix), matching `#pragma pack(push, 1)` and `static_assert` on the C++
side (60 and 64 bytes).

---

## 4. Wire format

The capture is **not** a bare sequence of CSI headers. Each frame is:

```
┌────────────────────────────┬──────────────────────────────┬──────────────────┐
│ HTTP frame prefix (60 B)   │ CSI record header (64 B)     │ CSI payload      │
│ RawCsiHttpFramePrefix      │ RawCsiRecordHeaderV8         │ csi_len_bytes    │
└────────────────────────────┴──────────────────────────────┴──────────────────┘
 offset 0                     offset 60                      offset 60+64 = 124
```

- `prefix.header_len` = 60 (its own size).
- `prefix.record_len` = size of **record header + payload** (not the prefix).
- Therefore `frame_len = prefix.header_len + prefix.record_len`
  (`csi_io.py` L947).
- In this capture `record_len = 64 + 128 = 192`, so `frame_len = 252`.

### 4.1 HTTP frame prefix — 60 bytes (`<IBBH16sQHHQQQ`)

Offsets are relative to the start of the frame.

| Off | Size | Rust | Python | Field | Meaning |
|---|---|---|---|---|---|
| 0 | 4 | u32 | `I` | `magic` | `0x52505345` = bytes `45 53 50 52` = `"ESPR"` |
| 4 | 1 | u8 | `B` | `version` | protocol version, must be `1` |
| 5 | 1 | u8 | `B` | `record_version` | must be `8` |
| 6 | 2 | u16 | `H` | `header_len` | size of this prefix, must be `60` |
| 8 | 16 | `[u8;16]` | `16s` | `session_id` | random id, constant for the whole connection |
| 24 | 8 | u64 | `Q` | `stream_sequence` | 1-based sequence of packets **offered** to the stream, including dropped ones |
| 32 | 2 | u16 | `H` | `record_len` | bytes of record header + payload that follow |
| 34 | 2 | u16 | `H` | `flags` | reserved; must be `0` (not the CSI flags!) |
| 36 | 8 | u64 | `Q` | `fresh_record_total` | records sent in session, including this one |
| 44 | 8 | u64 | `Q` | `raw_drop_total` | records not sent (queue full / invalid / batch failure) |
| 52 | 8 | u64 | `Q` | `raw_send_backpressure_total` | batch sends the client did not accept |

### 4.2 CSI record header — 64 bytes (`<HBBBBIHHQQIQBbbQIIBBB`)

Offsets are relative to the record start (frame offset 60). The payload starts
at `record_start + header_len` (normally +64).

| Off | Size | Rust | Python | Field | Meaning |
|---|---|---|---|---|---|
| 0 | 2 | u16 | `H` | `magic` | `0x4353` = bytes `53 43` = `"SC"` |
| 2 | 1 | u8 | `B` | `version` | record version, must be `8` |
| 3 | 1 | u8 | `B` | `header_len` | size of this header; must be `>= 64` |
| 4 | 1 | u8 | `B` | `chip_code` | `RawCsiChipType` |
| 5 | 1 | u8 | `B` | `flags` | bit set of `RawCsiRecordFlags` |
| 6 | 4 | u32 | `I` | `seq_num` | stream sequence, saturated at `u32::MAX` |
| 10 | 2 | u16 | `H` | `num_sc` | complex subcarriers = `csi_len_bytes / 2` |
| 12 | 2 | u16 | `H` | `csi_len_bytes` | payload size, `<= 512` |
| 14 | 8 | u64 | `Q` | `device_id` | device identity |
| 22 | 8 | u64 | `Q` | `device_ticks_us` | monotonic device time of the sample, µs |
| 30 | 4 | u32 | `I` | `wifi_rx_ts_us` | driver RX time, µs; valid only with flag bit 1 |
| 34 | 8 | u64 | `Q` | `wifi_rx_start_ts_ns` | RX start, ns; valid only with flag bit 2 (SDK writes 0) |
| 42 | 1 | u8 | `B` | `channel` | primary channel |
| 43 | 1 | i8 | `b` | `rssi_dbm` | RSSI, signed |
| 44 | 1 | i8 | `b` | `noise_floor_dbm` | signed; `-128` means "unavailable" |
| 45 | 8 | u64 | `Q` | `transport_counter` | copy of prefix `raw_send_backpressure_total` |
| 53 | 4 | u32 | `I` | `fresh_counter` | copy of prefix `fresh_record_total`, saturated |
| 57 | 4 | u32 | `I` | `request_counter` | copy of prefix `stream_sequence`, saturated |
| 61 | 1 | u8 | `B` | `phy_mode_code` | `RawCsiPhyMode` |
| 62 | 1 | u8 | `B` | `ltf_type_code` | `RawCsiLtfType` |
| 63 | 1 | u8 | `B` | `channel_width_code` | `RawCsiChannelWidth` |

### 4.3 Enumerations and flags

| Enum | Values |
|---|---|
| `chip_code` | 0 unknown, 1 ESP32, 2 S2, 3 S3, 4 C3, 5 C5, 6 C6 |
| `phy_mode_code` | 0 unknown, 1 legacy, 2 ht, 3 vht, 4 he-su, 5 he-mu, 6 he-ersu, 7 he-tb |
| `ltf_type_code` | 0 unknown, 1 lltf, 2 ht-ltf, 3 vht-ltf, 4 he-ltf |
| `channel_width_code` | 0 unknown, 1 20 MHz, 2 40, 3 80, 4 160, 5 80+80 |

| `flags` bit | Constant | Meaning |
|---|---|---|
| 0 (0x01) | `RAW_CSI_FLAG_FIRST_WORD_INVALID` | hardware marked first 4 payload bytes invalid (they are zeroed); in classic layout this hits DC and +1 |
| 1 (0x02) | `RAW_CSI_FLAG_WIFI_RX_TS_VALID` | `wifi_rx_ts_us` is usable |
| 2 (0x04) | `RAW_CSI_FLAG_WIFI_RX_START_TS_NS_VALID` | `wifi_rx_start_ts_ns` is usable |
| 3 (0x08) | `RAW_CSI_FLAG_FRESH` | always set on emitted records |

### 4.4 Payload

`csi_len_bytes` bytes of interleaved **signed 8-bit** samples. Reference
convention (`csi_io.py` L346–349, proven by the test at L180–206):

```
payload[0] = Q0, payload[1] = I0, payload[2] = Q1, payload[3] = I1, ...
complex[k] = I[k] + j*Q[k]      // Q first on the wire
amplitude[k] = hypot(I[k], Q[k])
phase[k]     = atan2(Q[k], I[k])
```

Test fixture sends `[10, 20, -30, 40]` and expects `[20+10j, 40-30j]` — proof
that even indices are Q and odd indices are I.

The payload is already normalized to the **centered HT20** convention:
64 complex bins, bin `k` ↔ physical subcarrier `k - 32`. Null bins are
`{0,1,2,3,32,61,62,63}` (guard edges + DC). The classic Espressif order
(`0..31, -32..-1`) would need a `roll(64)` correction, but the current
firmware does not emit it here. Do **not** reorder payload bytes at decode time.

---

## 5. Validation contract (mirror `_consume_raw_frames`)

Validation happens in two layers. The Rust decoder must reproduce both, but
report errors instead of raising on the first anomaly.

### 5.1 Envelope checks (per frame, `csi_io.py` L950–964)

1. `magic == 0x52505345`
2. `version == 1`
3. `record_version == 8`
4. `header_len == 60`
5. `session_id` equals the first frame's id (first frame seeds it)
6. `stream_sequence > previous stream_sequence` (strictly increasing)
7. `flags == 0`
8. `record_len != 0`
9. `record_len <= 64 + 512 = 576`
10. `fresh_record_total == previous fresh_record_total + 1`
11. `raw_drop_total >= previous raw_drop_total`
12. `raw_send_backpressure_total >= previous value`
13. `len(buffer) >= frame_len` else wait for more bytes (streaming)

### 5.2 Record checks (`csi_io.py` L404–412)

1. `magic == 0x4353`
2. `version == 8`
3. `header_len >= 64`
4. `csi_len_bytes != 0`
5. `csi_len_bytes == num_sc * 2`
6. `record_start + header_len + csi_len_bytes` fits in the frame
7. `next_offset == frame_len`, i.e. `prefix.record_len == csi_header_len + csi_len_bytes`

### 5.3 Cross-checks between layers (`csi_io.py` L980–989)

1. `csi.seq_num == min(stream_sequence, 0xFFFFFFFF)`
2. `csi.request_counter == min(stream_sequence, 0xFFFFFFFF)`
3. `csi.fresh_counter == min(fresh_record_total, 0xFFFFFFFF)`
4. `csi.transport_counter == raw_send_backpressure_total`

### 5.4 Semantics applied after parsing

- `wifi_rx_ts_us` → `None` unless flag bit 1 is set.
- `wifi_rx_start_ts_ns` → `None` unless flag bit 2 is set.
- `noise_floor_dbm == -128` → `None`.
- `device_id == 0` → `None`; `device_ticks_us == 0` → `None` (`or None` in Python).
- `flags & 0x08` → `csi_fresh`.
- `CSIPacket.timestamp` is **not** a wire field: the reference fills it with
  `time.time()` at parse time. Do not mistake it for a device timestamp.
- `seq_num` repeats when the device saturates at `u32::MAX`; a gap in
  `stream_sequence` means packets were dropped before send (also visible in
  `raw_drop_total` deltas), not necessarily parse failure.

For file decoding, seed "previous" values from the **first** frame rather than
assuming 1/0, because a capture may begin mid-session. Report the seed values.

---

## 6. Verified facts about `capture_v8.bin`

Measured with a throwaway script that mirrors the reference structs, and
cross-checked by importing the real `tools.lib.csi_io` parser (first record
decodes identically).

| Property | Value |
|---|---|
| File size | 174,132 bytes |
| Frame size | 252 bytes (60 + 192) |
| Frame count | **691**, zero trailing bytes |
| Session ID | `d95c15bbdae409e6afdc3191016cadbb` (constant) |
| Device ID | `0xb78d3a15354ad161` |
| Chip | 5 = **C5** |
| Frame flags | 10 = `0b1010` = `WIFI_RX_TS_VALID | FRESH` |
| CSI record flags | same value 10, on all 691 records |
| PHY / LTF / width | 3 / 3 / 1 = **VHT / VHT-LTF / 20 MHz** |
| Channel | 149 (5 GHz), constant |
| RSSI | -38 … -34 dBm |
| Noise floor | -94 dBm, constant |
| Subcarriers / payload | 64 / 128 bytes, constant |
| Header length | 60 and 64, constant |
| `stream_sequence` | 1 … 691, dense |
| `seq_num` (record) | 1 … 691, dense, matches stream sequence |
| `fresh_record_total` | 1 … 691, dense |
| Drop / backpressure counters | 0 throughout |
| Capture span | 34.499 s, ~19.99 PPS (median `device_ticks_us` delta 50,016 µs) |
| `device_ticks_us` | strictly increasing |
| Always-null bins | `{0,1,2,3,32,61,62,63}` → centered HT20 confirmed |
| Never-null bins | 4–31, 33–60 |
| Max abs I/Q sample | 38 |
| Payload SHA-256 | `840ba4c87683f055cb4e28537b84c4124f1968ccb7a3556c113405070e3b9b11` |
| File SHA-256 | `dd98ef43d7dbe9f9fd899398f91aa2bf36e85682e5bd79fc1c9e88c91b2ae938` |
| Sum of all raw I/Q bytes | -42,069 |
| Global amplitude mean / std | 24.768 / 10.369 |
| First record, bin 4 | raw `[Q=5, I=-20]`, amp 20.615528, phase 2.896614 |

### 6.1 Corrections to `plan.md`

- `plan.md` §3 claims the payload starts at `0x7C` with
  `05 ec 06 eb 06 eb 06 ea`. It **does** start at 0x7C (124), but the first
  four *complex* bins (8 bytes) are zeros: `0x7C..0x83`, and `05 ec …` begins
  at **0x84**. This is the centered guard-null pattern, not a header offset
  error. Layout is otherwise confirmed.
- `plan.md` §2.2 says "21 fields"; correct. But note the field named
  `channel` sits at offset 42 **after** the two 8-byte timestamps — easy to
  mis-order if copying from the C++ struct visually.

---

## 7. Implemented Rust design

### 7.1 Files

```
src/lib.rs          pub mod csi_header; pub mod decoder;
src/csi_header.rs   21-field CsiHeader + flag/enum constants + name helpers
src/decoder.rs      LeReader, HttpFrameHeader, CsiPacket, DecodeError,
                    FrameStreamDecoder, parse_record, 31 unit tests
src/bin/decode.rs   CLI: text/JSON report, --limit, --quiet
tests/capture_v8.rs 6 end-to-end tests over the real capture
```

`main.rs` (the live streamer) was left untouched.

### 7.2 Types and API

- `HttpFrameHeader` — 11 fields of §4.1, plus `parse(bytes, base)` and
  `frame_len()`. `WIRE_LEN = 60`.
- `CsiHeader` — 21 fields of §4.2. Methods: `first_word_invalid()`,
  `wifi_rx_ts_valid()`, `wifi_rx_start_ts_ns_valid()`, `is_fresh()`,
  `valid_wifi_rx_ts_us()`, `valid_wifi_rx_start_ts_ns()`,
  `valid_noise_floor_dbm()`, plus `chip_name()` / `phy_mode_name()` /
  `ltf_type_name()` / `channel_width_name()`.
- `CsiPacket { frame, csi, iq_raw: Vec<i8> }` with `iq()`, `iq_pairs()`
  (yields `(I, Q)` from the wire order `[Q, I]`), `amplitudes()`, `phases()`,
  `subcarriers()`.
- `DecodeError` — 20 variants, each with an absolute byte `offset`:
  `Truncated`, `BadFrameMagic`, `UnsupportedProtocolVersion`,
  `UnsupportedRecordVersion`, `BadFrameHeaderLength`, `SessionChanged`,
  `StreamSequenceNotIncreasing`, `NonZeroFrameFlags`,
  `RecordLengthOutOfRange`, `FreshTotalNotSequential`,
  `DropCounterWentBackwards`, `BackpressureCounterWentBackwards`,
  `BadRecordMagic`, `RecordHeaderTooShort`, `ZeroPayloadLength`,
  `PayloadLengthMismatch`, `PayloadTooLarge`, `RecordLengthMismatch`,
  `CounterMismatch`, `TrailingBytes`. Implements `Display` and
  `std::error::Error`.
- `FrameStreamDecoder` — `push(&[u8])`,
  `next_frame() -> Result<Option<CsiPacket>, DecodeError>`, `finish()`,
  `session_id()`, `frames_decoded()`, `bytes_consumed()`, `buffered_len()`.
  Handles frames split across reads and several frames per read; suitable for
  both files and the future live socket path.

### 7.3 Parsing rules as implemented

- `LeReader` performs explicit little-endian reads
  (`u16::from_le_bytes`, …) with bounds checks. The buffer is never cast into
  a Rust struct.
- The envelope is validated before waiting for the complete frame (fail fast),
  exactly like `_consume_raw_frames()`.
- `stream_sequence` and `fresh_record_total` state is seeded from the first
  frame's requirements (1 and 1 respectively; a capture starts at session
  start). Drop/backpressure counters start at 0.
- The record is parsed only from one complete frame; its declared size must
  equal `prefix.record_len`.
- All four envelope↔record counter cross-checks are enforced, including
  `min(u32::MAX)` saturation.
- On error, bytes are left in the buffer and parsing stops. There is no magic
  scan / silent resynchronization. `finish()` distinguishes a truncated final
  frame (`Truncated`) from stray bytes (`TrailingBytes`).

### 7.4 CLI

```bash
cargo run --bin decode -- capture_v8.bin              # per-record table + summary
cargo run --bin decode -- capture_v8.bin --limit 10   # cap printed rows
cargo run --bin decode -- capture_v8.bin --json       # JSON Lines
cargo run --bin decode -- capture_v8.bin --quiet      # summary only
```

Text summary includes frame count, bytes, a whole-payload FNV-1a 64 checksum,
session id, device id, sequence range, gaps/missing, drop and backpressure
deltas, duration from device ticks, and records/s. JSON mode emits
`{"type":"record",…}` lines and a final `{"type":"summary",…}` object (with
`payload_fnv1a64` as a hex string); payload arrays are intentionally not
dumped.

Example failure output:

```
error: input ended with undecoded data: truncated HTTP frame body at offset 99792: need 252 bytes, have 208
error: decode failed after 1 frame(s), at byte 252: bad HTTP frame magic at offset 252: 0x52505300
```

### 7.5 Test coverage

- 31 unit tests: one valid frame; I/Q order (`[10,20,-30,40] → [20+10j,40-30j]`);
  split frame; two frames per push; every error variant; noise-floor sentinel.
- 6 integration tests: 691 records; first-record byte-level fields; payload sum
  `-42_069`; FNV-1a 64 `0x6446_fc78_e2c7_f3e6`; bin 4 amplitude/phase; null
  bins; dense sequence; 37-byte chunked input.
- `scripts/verify_decoder.py` compares the CLI's JSON with ESPectre's Python
  parser: 15,899 field comparisons, 0 mismatches.
- `cargo clippy --lib --bins --tests` is clean.

### 7.6 Verification results

- Rust vs ESPectre Python (`tools.lib.csi_io.parse_csi_record`): **691/691
  records, 0 metadata mismatches**, 88,448 payload bytes compared by hash/sum.
- File: 691 frames, 174,132 bytes, zero trailing data.
- Demo run: 34.499 s, 20.00 records/s, 0 gaps, 0 drops, 0 backpressure.

### 7.7 Still open

- Wiring `FrameStreamDecoder` into the live `main.rs` stream.
- Classic-vs-centered layout auto-detection (not needed for this capture).
- A binary/`.npz` export of the I/Q arrays for the DSP stage.

---

## 8. Progress log

- [x] Read `README.md`, `plan.md`, all `src/*.rs`, `Cargo.toml`.
- [x] Read ESPectre C++ framing headers, Python reference parser, docs, tests.
- [x] Reverse-verify every byte of `capture_v8.bin` (691/691 frames valid).
- [x] Document field map, offsets, enums, flags, validation order.
- [x] Cross-check with the real Python parser (first record identical).
- [x] Implement `src/decoder.rs` (types, LE helpers, envelope + record parse).
- [x] Wire module into build (`src/lib.rs`) and add CLI (`src/bin/decode.rs`).
- [x] Add 31 unit + 6 integration tests, including a Python-parity check.
- [x] Produce the metadata report (`cargo run --bin decode -- capture_v8.bin`).
- [x] Verify all 691 records against Python (15,899 comparisons, 0 mismatches).
- [x] Add `scripts/verify_decoder.py` to make that check repeatable.
- [x] `cargo test` (37 passing) and `cargo clippy` (clean).

## 9. Next step

The V8 decoder is done and validated. Remaining work, in order:

1. Wire `FrameStreamDecoder` into `src/main.rs`: push each network chunk, then
   drain `next_frame()`. Keep writing the raw `capture_v8.bin` for reference.
2. Convert decoded packets into the numeric arrays the DSP stage expects
   (`csi_data` as `N × 128` `i8`, `device_ticks_us`, metadata) and write `.npz`.
3. Feed amplitudes to the web heatmap described in `README.md`.
4. Optional: use the ESPectre Python parser as a fuzz oracle for more captures.

Do not start DSP/AI work until the live path uses this decoder.
