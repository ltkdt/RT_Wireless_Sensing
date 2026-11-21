# Memory — RT-Wireless-Sensing

Living notes for the project: **design decisions and verified facts** that are not
obvious from any single source file. Per-module usage and API reference live in
`docs_EN/` (English) and `docs_VN/` (Vietnamese) and in rustdoc — do not duplicate
them here. This file is not a changelog.

> `PLAN.md` holds the current feature plan: the **Diagnostic view** for the
> visualization module (PPS-deficit diagnosis, approved 2026-11-19). The old
> DSP-rewrite plan is obsolete — DSP is implemented in `src/dsp/` and documented in
> `docs_EN/dsp.md` / `docs_VN/dsp.md`.

Last updated: 2026-11-19 (diagnostic view phase 2 implemented: real per-record
data, empty-slot classification, and per-second statistics computed in Rust)

---

## 1. Current project state

Rust crate (edition 2024) for real-time Wi-Fi CSI sensing with an ESPectre ESP32.
The firmware is external ([francescopace/espectre](https://github.com/francescopace/espectre))
and is not modified here.

```
src/
  lib.rs            pub mod collection, csi_header, decoder, saving, dsp, visualization, streaming
  collection.rs     ESPectre HTTP API + UDP markers + raw .bin capture   (bin: collect, default main)
  csi_header.rs     64-byte V8 record schema (shared with decoder)
  decoder.rs        incremental V8 frame parser/validator               (bin: decode)
  saving.rs         .bin -> .npz export                                 (bin: save)
  dsp/              offline signal processing, library only             (mod, preprocess, resample, spectral, filter)
  visualization.rs  heatmap data prep + standalone HTML / HTTP server   (bin: visualize)
  streaming.rs      collect + live decode + dashboard in one process    (bin: streaming)
web/                static frontend: index.html, style.css, visualizer.js (Canvas 2D, Viridis/Plasma/Jet)
scripts/            verify_decoder.py (Rust-vs-Python differential check), inspect_espectre_data.py
tests/              capture_v8.rs, dsp.rs, saving.rs, visualization.rs (integration)
samples/            espectre_data/*.npz (labeled: free_space, standstill, wavehand_vert), raw_record/*.bin
docs_EN/, docs_VN/  per-module docs in two languages
artifacts/          git-ignored: capture_v8.bin, 50pps_test.bin, heatmap HTML, decoder output
```

Pipeline: ESP32 → `collection` writes raw bytes → `decoder` parses frames →
`saving` exports `.npz` → `dsp` processes offline; `visualization`/`streaming`
render heatmaps (offline file or live).

**Test data caveat (verified):** integration tests and the visualization unit tests
read `capture_v8.bin` from the repo root (`CARGO_MANIFEST_DIR`), but the file is not
tracked — a copy lives in the git-ignored `artifacts/`. Run
`cp artifacts/capture_v8.bin .` before `cargo test --all-targets`, or those tests
fail with "capture_v8.bin should exist".

---

## 2. Design decisions (keep these invariants)

### 2.1 Module ownership / layering

- **Collection never parses bytes.** It writes exactly the `GET /csi` response body
  to the `.bin`; `decoder` is the only parser ("one decoder, many consumers").
- **DSP never re-parses `.bin`.** It consumes arrays from `saving` (`.npz`) or a
  thin `CsiSignal` loader over the decoder. No second decode path.
- The **60-byte HTTP frame prefix** and the **64-byte CSI record header** are
  different structures and must not be confused (offsets, magics, counters differ).
- `streaming` composes the modules instead of duplicating them: it uses
  `collection::collect_with_sink`, whose per-chunk callback feeds a live
  `FrameStreamDecoder` while the raw `.bin` is still written.

### 2.2 Collection behavior

Sequence per run: `GET /capabilities` → `PATCH /sensing` with
`{"traffic_generator_mode": "external"}` → spawn UDP marker sender (4-byte marker
`F0 9F 91 BB`, the "👻" emoji, at `pps`) → wait `startup_delay` → `GET /csi` and
copy chunks verbatim to `output`.

- Cancellation: the marker task is held in an `AbortOnDrop` guard, so dropping the
  `collect` future aborts both the HTTP request and the marker task.
- `max_duration: Option<Duration>` (default `None` = until the device closes the
  stream). When set, the streaming loop races each chunk against a deadline and
  returns normally with the `CollectionSummary` (`duration` ≈ the limit). CLI:
  `collect --duration 20` (bare number = seconds; `ms`/`s`/`m`/`h` suffixes work).
- `validate()`: `pps > 0` and the marker endpoint must parse.

### 2.3 Decoder rules

- Fail-fast, two-layer validation (envelope, then record) plus envelope↔record
  counter cross-checks. **No magic-scan / silent resynchronization**: on error the
  bytes stay buffered and parsing stops.
- `FrameStreamDecoder` is streaming-safe (`push` / `next_frame` / `finish`); frames
  may be split across reads or several per read. `finish()` distinguishes a
  truncated final frame (`Truncated`) from stray bytes (`TrailingBytes`).
- Seed "previous" counters from the **first** frame (a capture may begin
  mid-session); enforce strictly increasing `stream_sequence`,
  `fresh_record_total == prev + 1`, monotone drop/backpressure counters, and the
  saturated `min(u32::MAX)` cross-checks.
- Semantics after parsing: `wifi_rx_ts_us` / `wifi_rx_start_ts_ns` are `None` unless
  their flag bits are set; `noise_floor_dbm == -128` → `None`; `device_id == 0` and
  `device_ticks_us == 0` → `None`.

### 2.4 Saving

`.bin` → same-stem `.npz`: `timestamp_ms` (milliseconds relative to the first
record, from `device_ticks_us`) plus `amplitude_00..63` and `phase_00..63` —
**129 arrays for 64 subcarriers**, each of length N frames.

### 2.5 DSP principles

- Synchronous and pure: no tokio, no network, no file I/O inside DSP functions;
  deterministic transforms over `ndarray`.
- `CsiSignal { amplitude, phase, ticks_us, fs }` is the composable core type;
  `fs` comes from the median tick interval. Errors are a `DspError` enum — no
  `Box<dyn Error>` in public signatures.
- Dependency policy: prefer a maintained crate that adds the function without
  async/network baggage; otherwise hand-roll and unit-test. Chosen: `realfft`
  (one-sided real FFT), `biquad` (RBJ LPF/HPF/BPF), `ndarray`/`ndarray-npy`.
- Resample the **whole record at once** on a uniform grid derived from cumulative
  ticks — never per-second chunks (boundary kinks). Interpolate **I/Q**, then
  recompute amplitude/phase; never interpolate amplitude or raw phase. Methods:
  Linear / Nearest / natural cubic spline (spline recommended for spectral work).
- FFT is one-sided with `2 / window_sum` amplitude scaling; windows
  None/Hann/Hamming/Blackman; STFT default Hann with 50% overlap. Window guidance:
  ~10–20 s for breathing (0.1–0.5 Hz), ~0.5–1 s for motion/gestures (0.2–10 Hz).
- Outlier removal per subcarrier: moving / exponential-moving / median / Hampel;
  typical chains: BPF 0.1–0.5 Hz for breathing, HPF to strip static reflections
  before gesture analysis.
- DWT (wavelets) and PCA/compression are explicitly out of scope for `dsp`.

### 2.6 Visualization / streaming

- `web/` is a self-contained static frontend; `visualization.rs` either embeds it
  into one standalone `.html` output or serves it over a Tokio HTTP server.
- `streaming` defaults: port **8080**, warm-up **3 s**, refresh **500 ms**, sliding
  window **30 s** with absolute time labels. Clicking the heatmap reports occupancy
  of that second as `N_filled / N` slots, where `N` = configured PPS
  (`metadata.nominal_pps` drives the slot count).
- **Diagnostic view (phase 2, implemented).** `VisualizationData.diagnostics`
  carries `DiagnosticTrace { slots_per_second, origin_ticks, capture_end,
  records[], empty_slots[], seconds[], summary }`. Invariants worth keeping:
  - Slot assignment is integer math on device ticks
    (`within_us * slots / 1_000_000`), the exact form of the browser occupancy
    formula `floor((t − second_start) * slots + 1e-6)`; a test asserts both agree
    per second on `capture_v8.bin`.
  - All deltas, the empty-slot classification (`DiagnosticCause` → layer), and
    the per-second statistics are computed in Rust, so the same numbers can be
    exported with `--json`. The web layer only formats them.
  - `capture_end` distinguishes an offline capture from a live window edge;
    trailing slots without a closing record stay `ambiguous` either way, and
    `seconds[].partial` marks a second the trace does not fully cover.
  - Offline files need `--pps` (`from_file_with_pps`); without it the grid falls
    back to `effective_pps` and shrinks exactly for the seconds that miss samples.
  - `summary` (capture overview) covers the **covered span only** — first to last
    received record — so `total_slots == filled_records + empty_slots` and the
    five `cause_counts` buckets partition `empty_slots`. `ambiguous` absorbs the
    capture edge and the live-window edge; it is never folded into another cause.
    The field is `#[serde(default)]`, and the web layer re-aggregates
    `empty_slots` when it is absent.
  - Cost: a 34 s capture at 100 slots adds ~1 MB of diagnostics to the payload.
  - **Live streaming is opt-in (implemented).** Offline payloads (`visualize`,
    `from_file*`, `prepare_visualization_data*`) always carry `diagnostics`
    (`VisualizationOptions::diagnostics = true` by default). Streaming defaults to
    **no** trace (`StreamingConfig::diagnostics = false`) and only builds it with
    `--diagnostics`, saving ~1 MB per 500 ms poll. The web layer hides the toggle
    when the field is `null`; occupancy is unaffected (browser-side timestamps).
  - Test policy: the diagnostic tests that constructed `VisualizationOptions`
    directly were removed rather than migrated — mapping the flag is the only
    thing needed from them. `diagnostic_trace_is_optional` in
    `tests/visualization.rs` is the contract for this change.

#### Opt-in rule for computed view features (project-wide)

Any viewing feature whose data must be **computed** ships **off by default in
streaming mode** and is turned on only by an explicit CLI argument. Reason: the
server re-publishes its whole payload every 500 ms while streaming, so every
computed block is paid for on every poll whether the user ever opens the panel or
not — the diagnostic trace alone costs ~1 MB per poll (34 s × 100 slots).

- **Offline is exempt**: `visualize` / `from_file*` produce one payload for one
  file, so the cost is paid once. Computed data defaults to **on** there (this is
  why `VisualizationOptions::diagnostics` defaults to `true` while
  `StreamingConfig::diagnostics` defaults to `false`).
- **Implement it as a payload switch, not a web switch.** The field is `None` /
  `null` when disabled and the web layer hides the panel; it never recomputes the
  numbers browser-side. This keeps one implementation of every formula (Rust),
  and the hiding rule is already established (`web/visualizer.js` gates panels on
  the presence of their payload field).
- Enablement is a boolean on `StreamingConfig` + a `--<feature>` flag in
  `src/bin/streaming.rs`; document it in `docs_EN`/`docs_VN` `streaming.md`.
- Disabling must not change the base payload: the heatmap matrix, timestamps and
  metadata are identical either way (asserted in `diagnostic_trace_is_optional`).
- **Planned application: a DSP panel** (driven by the `dsp` module — FFT/STFT,
  filtering, resampling) is in planning phase with no approved plan yet. When it
  lands it follows this rule: computed spectra/filter outputs off by default in
  streaming, enabled by a CLI flag; always present for offline files.

---

## 3. Defaults and constants (verified in source)

Collection (`src/collection.rs`):

| Constant | Value |
|---|---|
| `DEFAULT_ESP32_IP` | `10.178.22.9` (second recorded device: `10.178.22.62`) |
| `DEFAULT_API_PORT` | `62587` |
| `DEFAULT_MARKER_PORT` | `5555` |
| `DEFAULT_PPS` | `20` |
| `DEFAULT_ORIGIN` | `https://test.espectre.dev` |
| `DEFAULT_OUTPUT` | `capture_v8.bin` |
| `startup_delay` | `200 ms` |
| `max_duration` | `None` (unlimited) |
| `verbose` | `true` |

Streaming (`src/streaming.rs`): `DEFAULT_PORT = 8080`,
`DEFAULT_WARMUP = 3 s`, `DEFAULT_UPDATE_INTERVAL = 500 ms`,
`DEFAULT_WINDOW = 30 s`.

---

## 4. Sources of truth

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

## 5. Wire format (verified)

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
- In the reference capture `record_len = 64 + 128 = 192`, so `frame_len = 252`.

### 5.1 HTTP frame prefix — 60 bytes (`<IBBH16sQHHQQQ`)

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

### 5.2 CSI record header — 64 bytes (`<HBBBBIHHQQIQBbbQIIBBB`)

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

### 5.3 Enumerations and flags

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

### 5.4 Payload

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

## 6. Validation contract (mirrors `_consume_raw_frames`)

Validation happens in two layers; the decoder reproduces both and reports errors
instead of raising on the first anomaly.

### 6.1 Envelope checks (per frame, `csi_io.py` L950–964)

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

### 6.2 Record checks (`csi_io.py` L404–412)

1. `magic == 0x4353`
2. `version == 8`
3. `header_len >= 64`
4. `csi_len_bytes != 0`
5. `csi_len_bytes == num_sc * 2`
6. `record_start + header_len + csi_len_bytes` fits in the frame
7. `next_offset == frame_len`, i.e. `prefix.record_len == csi_header_len + csi_len_bytes`

### 6.3 Cross-checks between layers (`csi_io.py` L980–989)

1. `csi.seq_num == min(stream_sequence, 0xFFFFFFFF)`
2. `csi.request_counter == min(stream_sequence, 0xFFFFFFFF)`
3. `csi.fresh_counter == min(fresh_record_total, 0xFFFFFFFF)`
4. `csi.transport_counter == raw_send_backpressure_total`

### 6.4 Semantics applied after parsing

- `wifi_rx_ts_us` → `None` unless flag bit 1 is set.
- `wifi_rx_start_ts_ns` → `None` unless flag bit 2 is set.
- `noise_floor_dbm == -128` → `None`.
- `device_id == 0` → `None`; `device_ticks_us == 0` → `None`.
- `flags & 0x08` → `csi_fresh`.
- `CSIPacket.timestamp` is **not** a wire field: the reference fills it with
  `time.time()` at parse time. Do not mistake it for a device timestamp.
- `seq_num` repeats when the device saturates at `u32::MAX`; a gap in
  `stream_sequence` means packets were dropped before send (also visible in
  `raw_drop_total` deltas), not necessarily a parse failure.

---

## 7. Verified facts about the reference capture

`capture_v8.bin` (copy in `artifacts/`, recorded from an ESP32-C5 at 20 PPS),
measured with a script mirroring the reference structs and cross-checked against
ESPectre's real `tools.lib.csi_io` parser:

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
| Payload FNV-1a 64 (locked by `tests/capture_v8.rs`) | `0x6446fc78e2c7f3e6` |
| Global amplitude mean / std | 24.768 / 10.369 |
| First record, bin 4 | raw `[Q=5, I=-20]`, amp 20.615528, phase 2.896614 |

**Cross-validation results:** Rust decoder vs ESPectre Python
(`tools.lib.csi_io.parse_csi_record`): **691/691 records, 0 metadata mismatches**;
`scripts/verify_decoder.py` made 15,899 field comparisons with 0 mismatches.
Demo decode run: 34.499 s, 20.00 records/s, 0 gaps, 0 drops, 0 backpressure.

---

## 8. Testing

```bash
cp artifacts/capture_v8.bin .   # tests expect it at the repo root
cargo test --all-targets
cargo clippy --all-targets
```

- `tests/capture_v8.rs` locks the whole-file decode of the real capture (691
  records, payload FNV `0x6446fc78e2c7f3e6`, byte-level first-record fields).
- `tests/saving.rs` verifies the `.npz` layout and golden values;
  `tests/dsp.rs` and `tests/visualization.rs` cover the other modules.
- `scripts/verify_decoder.py` re-runs the Rust-vs-Python differential check.

---

## 9. Open / future work

- ONNX inference for localization is **not implemented**; it should consume
  decoded packets or the `.npz` arrays (README pipeline step 7).
- Rendering DSP output (e.g. `Spectrogram` as a heatmap) inside the visualization
  dashboard is a possible follow-up; `Spectrogram`/`Spectrum` are plain-data structs
  so the visualization layer does not depend on dsp internals.
- More captures can be validated with the ESPectre Python parser as a fuzz oracle.
