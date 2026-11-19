# PLAN.md — Diagnostic View (Visualization Module)

Status: approved. Every code claim below was verified against the current tree
(references inline). This file replaces the deprecated DSP-rewrite plan; the DSP
module is implemented in `src/dsp/` (see `docs_EN/dsp.md`).

## 1. Goal and scope

Goal: diagnose directly on the dashboard why each second yields fewer CSI samples
than the configured PPS (the ~70/100 pps and ~45/50 pps problem), down to per-slot
resolution.

Scope constraints:

- Every displayed piece of information must already exist in the `.bin` file
  (60-byte HTTP prefix `decoder.rs:44` + 64-byte CSI record header
  `csi_header.rs:24`). No firmware change, no generator-side log.
- No decoder change; visualization reads `CsiPacket` directly — it already carries
  both layers (`frame: HttpFrameHeader`, `csi: CsiHeader`, `decoder.rs:157-165`).
- Independent of the amplitude heatmap: a separate canvas behind a toggle, reusing
  the existing click-to-select-second and slot logic (`visualizer.js`
  `computeOccupancy`).
- Delivered in two phases (§9): **Phase 1** UI with placeholders, **Phase 2** real
  data.

## 2. Interaction flow — the two info boxes

1. A "Diagnostic view" toggle at the bottom, shown only when the payload has a
   `diagnostics` field (optional field → old payloads keep the toggle hidden).
   Phase 1 may ship a placeholder/demo trace; the final gate is the `diagnostics`
   field.
2. Clicking a heatmap point selects that second — same as the current occupancy
   mechanism; the slot logic is reused.
3. The diagnostic canvas draws the `subcarriers × slots` matrix of the selected
   second (`slots` = integer nominal PPS). Empty columns render distinctly
   (hatched/gray); filled columns use the amplitude colormap.
4. **Box 1 — slot hover tooltip.** Hovering a slot shows a tooltip that explains
   the *meaning of each field* displayed (e.g. `stream_sequence` = one-based count
   of packets offered to the stream, including dropped ones) plus the filled/empty
   cell content of §4/§5. It needs its own tooltip element — the existing
   `#heatmapTooltip` (`web/index.html:96`, styled at `style.css:279`) is a single
   instance bound to the heatmap canvas.
5. **Box 2 — statistics box for the selected second**, as in §6. The existing
   inline `Occupancy (click)` readout (`#hoverOccupancy`, `web/index.html:73`) is
   kept as is and untouched.

Slot definition: `slot_start(i) = second_start + i·(1_000_000 / nominal_pps)` µs in
device ticks; a record belongs to the half-open slot containing its
`device_ticks_us`. Consistency (verified): the existing JS occupancy computes slots
from `timestamps` in seconds as `floor((t − secondStart)·slots + 1e-6)` and rounds
`nominal_pps` to an integer (`getNominalPps` in `visualizer.js`). The diagnostic
grid must use the same integer slot count — Rust rounds `nominal_pps` once and the
column count is passed explicitly.

## 3. Data sources (verified against the wire format)

### 3.1 Envelope — `HttpFrameHeader` (`decoder.rs:70-96`)

| Field | Type | Meaning |
|---|---|---|
| `stream_sequence` | u64 | One-based sequence of packets **offered** to the stream, including dropped ones; a gap = frames lost at the device stream layer |
| `fresh_record_total` | u64 | Fresh records sent in the session incl. this frame; decoder enforces exactly +1 per received frame, so it never reveals a gap by itself |
| `raw_drop_total` | u64 | Records not sent (queue full / invalid packet / batch send failed) |
| `raw_send_backpressure_total` | u64 | Batches the client (desktop) did not accept — desktop reads too slowly / HTTP stack congested |
| `session_id` | `[u8; 16]` | Session identity; changing mid-run is a decode error (`decoder.rs:754-763`) |

Decoder enforcement (`decoder.rs:726-815`): `stream_sequence` must be strictly
increasing, so **gaps pass validation** (only non-increase is an error);
`fresh_record_total == prev + 1` exactly (first frame must be 1); drop/backpressure
counters may only grow.

### 3.2 CSI record header — `CsiHeader` (`csi_header.rs:77-123`)

| Field | Diagnostic meaning |
|---|---|
| `seq_num`, `request_counter` (u32, saturate at `u32::MAX`) | Saturated copies of `stream_sequence`; cross-checks only, no extra information |
| `fresh_counter` (u32) | Saturated copy of `fresh_record_total` |
| `transport_counter` (u64) | Copy of `raw_send_backpressure_total` |
| `device_ticks_us` | Sampling instant on the device monotonic clock (µs); basis of all intervals and the slot offset |
| `wifi_rx_ts_us` | Driver RX timestamp, valid only with `WIFI_RX_TS_VALID`; different clock domain → only relative Δ usable |
| `wifi_rx_start_ts_ns` | RX-start timestamp, valid only with flag bit 2; SDK currently writes 0 (`csi_header.rs:48-50`) |
| `rssi_dbm`, `noise_floor_dbm` | Per-record radio quality. The `-128` "unknown" sentinel is defined **only for `noise_floor_dbm`** (`NOISE_FLOOR_INVALID_DBM`, `csi_header.rs:30-33`); `rssi_dbm` has no sentinel |
| `flags` | `FRESH`, `FIRST_WORD_INVALID`, `WIFI_RX_TS_VALID`, `WIFI_RX_START_TS_NS_VALID` (`csi_header.rs:44-56`) |
| `channel`, `phy_mode_code`, `channel_width_code`, `chip_code` | Normally constant per session; a mid-run change is an environment/buffering signal worth a warning |

Key conclusion (kept from the draft; note it is firmware-side and **not verifiable
from this repo**): the ESP32→desktop link is HTTP over TCP, so in-transit loss is
impossible. A `stream_sequence` gap = loss at the device stream layer; an empty
slot with contiguous `stream_sequence` and unmoved counters = no record was ever
generated (marker lost over the air, or firmware produced no CSI).

### 3.3 Layer model — which side is at fault

All counters are *recorded on* the RX device (they live in its frame prefix), but
they attribute the *cause* to different layers. This distinction must be stated
explicitly in the tooltip and docs:

- **Generator (desktop):** sends the UDP markers at `--pps` and reads the HTTP
  stream. Marker not sent or jittered → "no record generated" with contiguous
  counters; slow reading → `raw_send_backpressure_total` rises; steadily increasing
  slot offset → generator jitter.
- **TX (AP/router) + airtime:** the marker frame crosses the air to the device.
  Loss here → empty slot with contiguous `stream_sequence`, unmoved counters, often
  low neighboring RSSI.
- **RX (ESP32 device):** produces and sends the records. `raw_drop_total` rising =
  the device did **not send** the record (queue full / invalid packet / batch send
  failed). A raw_drop is an **RX-side** drop — the record never left the device —
  which is clearly different from the **desktop failing to receive/accept** a batch
  (`raw_send_backpressure_total` rising). Same symptom (record not in the file),
  opposite sides.

## 4. Box 1 hover content — filled cell (metadata belongs to the whole column; only amplitude is per subcarrier)

Each row is rendered with a one-line field meaning and, where relevant, a layer tag
(the Box 1 requirement):

- Identity: run-wide record index, `seq_num`, slot index / slots, selected second.
- Time: absolute and capture-relative `device_ticks_us`; slot offset (µs) =
  `device_ticks_us` − `slot_start` (sign tells late/early vs the theoretical grid).
- Intervals: Δticks vs previous record (µs) and Δ`wifi_rx_ts_us`; if Δ(driver) is
  stable while Δ(device) fluctuates, the delay is after the driver.
- Stream layer (precomputed in Rust so JS tracks no state):
  - Δ`stream_sequence` > 1 with Δ`raw_drop_total` > 0 → tag **"RX (device): dropped
    before sending"** (full queue / invalid / batch failed)
  - Δ`raw_send_backpressure_total` > 0 → tag **"Desktop: client did not accept the
    batch"** (desktop reads too slowly)
- Radio quality: `rssi_dbm`, SNR = `rssi_dbm` − `noise_floor_dbm`, `noise_floor_dbm`
  ("unknown" for the sentinel). Low `rssi_dbm` on neighboring records → hint
  **"TX/airtime"**.
- Flags as labels: fresh / first-word-invalid / rx-ts-valid.
- Configuration: channel, PHY mode, channel width.
- Amplitude of the hovered subcarrier.

## 5. Box 1 hover content — empty cell

- Slot index, theoretical slot time on the device clock.
- Classification inferred from the two surrounding records, with a layer label.
  Since the decoder enforces Δfresh == 1 between consecutively received frames, the
  gap condition simplifies to **Δstream > 1**:
  - Δstream > 1, Δdrop > 0 → "dropped at device stream layer — **RX (device)**",
    with the number of dropped frames and Δ`raw_drop_total` / Δbackpressure at the
    boundary (full queue vs batch failure).
  - Δstream > 1, Δbackpressure > 0 → "**Desktop (generator/client)** reads too
    slowly".
  - Δstream == 1, counters unmoved, low neighboring RSSI → "over-air loss — **TX
    (AP) → RX airtime**"; show neighboring RSSI for comparison.
  - Δstream == 1, counters unmoved, normal RSSI → "marker not sent — **generator
    (desktop)**, or firmware produced no CSI".
  - "Ambiguous" when no following record closes the boundary. Live caveat
    (verified): streaming drops records outside the 30 s window before publishing
    (`streaming.rs:455-461`), so Rust must distinguish **capture end** from
    **window end** (pass a flag), else every window edge classifies as ambiguous.
- Both boundaries: `seq_num` and `device_ticks_us` of the immediately preceding and
  following records.

## 6. Box 2 — statistics box for the selected second (kept as planned)

Computed in JS from the `DiagnosticRecord` rows of that second (no counter state
machine in JS):

- Counts: samples / nominal_pps, effective pps of the second, empty slots, loss
  ratio.
- Δtick intervals: mean, median, min, max, std (jitter), p95 vs `1e6/pps`.
- Burst analysis: histogram of consecutive empty slots (1, 2, 3+). Scattered singles
  → random over-air loss; long clusters + rising Δbackpressure/Δdrop → buffering
  congestion.
- Slot offset (lateness): mean, max, per-slot sparkline. Steady increase → generator
  jitter; large random fluctuation → firmware load.
- Layer counters: total Δstream − count (frames dropped at stream), Δ`raw_drop_total`,
  Δ`raw_send_backpressure_total`, count of frames with backpressure > 0.
- Radio quality: min/mean/max RSSI in the second; flag empty slots coinciding with
  low RSSI.
- Anomalies: records missing FRESH, first-word-invalid count, channel/phy/width
  changes within the second.

## 7. Sample-loss classification rules (verbatim in the docs)

All counters are stored on the RX device, but each points at a different layer — a
`raw_drop_total` increase is an RX-side drop (the record never left the device),
**not** a desktop receive failure; that is a backpressure increase.

| Signature | Layer | Conclusion | Next action |
|---|---|---|---|
| Empty slot, `stream_sequence` gap, Δ`raw_drop_total` > 0 | **RX (device)** — record not sent (queue full / invalid / batch fail) | Device drop due to full queue | Lower PPS, increase batch size, check firmware CPU |
| Empty slot, gap, Δbackpressure > 0 | **Desktop** — client did not accept the batch | Desktop reads too slowly | Increase client buffer, slow the 500 ms refresh |
| Empty slot, contiguous counters, low neighboring RSSI | **TX/airtime** (AP → device) | Over-air loss (airtime) | Change channel, reduce other traffic, lower PPS |
| Empty slot, contiguous counters, normal RSSI | **Generator (desktop)** | Marker not sent/received | Corroborate with the sent-marker log |
| Sample present, large slot offset, high interval std | **Generator (desktop)** jitter | Timing jitter | Measure the generator |

## 8. Code changes

### Rust (`src/visualization.rs`)

- `DiagnosticRecord`: all derived metrics precomputed in Rust (Δstream, Δdrop,
  Δbackpressure, Δwifi_rx, slot offset once the second is known), serialized
  compactly. `DiagnosticTrace { enabled_origin_ticks, per_pps_hint,
  records: Vec<DiagnosticRecord> }` attached to `VisualizationData` as
  `diagnostics: Option<DiagnosticTrace>` — Option fields deserialize as `None` on
  old payloads, same pattern as `classification` (`visualization.rs:88-97`).
- `visualize` CLI: add `--pps` (verified absent — `bin/visualize.rs:145-207` has
  only `-o/--output`, `--json`, `--serve`, `--port`, `--open`, `-q`). Offline files
  currently fall back to `effective_pps` (`visualization.rs:318-322`), which
  produces a wrong slot grid exactly for seconds that miss samples. Implementation:
  decode via `decode_packets` + the already-`pub`
  `prepare_visualization_data_with_origin`, or add a `pps` parameter to
  `from_file`/`from_bytes`.
- **Subcarrier constant:** define one `STANDARD_SUBCARRIERS: usize = 64` in
  `csi_header.rs` (it owns the wire schema); delete the duplicated, currently-unused
  copies at `visualization.rs:41` and `saving.rs:58`. Verified: the decoder does
  **not** enforce 64 — only `csi_len_bytes == num_sc * 2` and payload ≤ 512, i.e.
  `num_sc ≤ 256` (`decoder.rs:972`, `csi_header.rs:40-43`). Visualization already
  enforces per-capture consistency (`SubcarrierCountChanged` in
  `prepare_visualization_data_with_origin`) and sends `metadata.subcarriers`; `dsp`
  has no hardcoded 64.

### Web (`web/`)

- Replace hardcoded 64s with `metadata.subcarriers` (always sent by Rust):
  `visualizer.js:355` (`|| 64`), `visualizer.js:557` (fallback `64`),
  `index.html:38` ("64 subcarriers / frame" label; keep a JS-side constant only as
  label fallback).
- Toggle + second canvas (hidden by default) drawing the `subcarriers × slots`
  matrix of the selected second, empty columns distinct.
- Separate slot-tooltip element (mousemove on the slot canvas): field-meaning lines
  (with layer tags) + filled/empty variants.
- Stats box as plain DOM, shown only when diagnostics are enabled and a second is
  selected.

### Streaming (`src/streaming.rs`)

- No logic change: `nominal_pps` is already passed (`streaming.rs:418, 463-467`);
  window 30 s / publish 500 ms (`streaming.rs:52-58`); the browser polls `/api/data`
  every 500 ms. Build the trace from the same window slice, but pass the window-end
  vs capture-end flag for §5. JSON size: 30 s × 100 pps ≈ 3000 records × ~12
  numbers — small next to the amplitude matrix already sent.

## 9. Phasing

- **Phase 1 — UI with placeholders.** Subcarrier-constant cleanup; toggle + slot
  canvas (filled/empty columns derivable from `timestamps` via the existing
  occupancy slot logic); slot tooltip with field meanings + placeholder values;
  stats box with placeholder text. Heatmap untouched when the toggle is off.
  Deliverable: reviewable UI.
- **Phase 2 — real data.** `DiagnosticRecord` + Δ computation in Rust; `diagnostics`
  payload field; `--pps` flag; slot assignment + classification in Rust (incl.
  window-end flag); real tooltip values; per-second stats from the records.

## 10. Documentation

- `docs_EN/visualization.md` + `docs_VN/visualization.md` mirror: new "Diagnostic
  view" section after the existing occupancy section (`docs_EN/visualization.md:40-54`):
  purpose; enabling/flow; slot-grid semantics + the `--pps` fallback warning for
  offline files; per-record field table (source, unit, what it discriminates;
  counters are session-cumulative, so every Δ is between consecutively received
  frames); derived-metrics table with limitations (`wifi_rx_ts_us` is another clock
  domain → relative Δ only; `seq_num` saturates at `u32::MAX`); classification table
  §7 verbatim; limitations (no marker identity; TCP → no "lost in transit" class;
  device clock only); short FAQ (why occupancy is low while `stream_sequence` stays
  contiguous; why the deficit grows with PPS).
- New "Which layer is at fault?" subsection: the three-layer map of §3.3
  (generator desktop / TX AP+airtime / RX device), the counter→layer mapping, and
  the explicit distinction that `raw_drop_total` is an RX-side drop (record never
  left the device) while `raw_send_backpressure_total` is a desktop-side receive
  failure.
- The Box 1 tooltip field-meaning lines include the layer tag, e.g.
  `Δraw_drop_total` = "records the device (RX) did not send — queue full / invalid /
  batch failed; not a desktop receive problem", and
  `Δraw_send_backpressure_total` = "batches the desktop did not accept —
  desktop-side cause".
- FAQ gains: "why does `raw_drop_total` rise while the desktop received everything?"
  → RX-side queue/batch drop.
- Rustdoc for `DiagnosticTrace`/`DiagnosticRecord` with cross-links to the decoder's
  counter definitions.
- README module-table row (`README.md:40`): add "diagnostic slot view for PPS
  deficit analysis".

## 11. Testing

- Parameterize `device_ticks_us` in the decoder test builder — `build_record`
  hardcodes `1_000_000` (`decoder.rs:~1140`); `build_frame(stream_sequence, fresh,
  drop, backpressure, payload)` is at `decoder.rs:1158` — so synthetic stream gaps
  and empty slots can be built; verify the classification.
- `capture_v8.bin` (691 frames, `tests/capture_v8.rs:30-32`; git-ignored, copy from
  `artifacts/` first — `Memory.md` §1): trace produced; per-second empty-slot count
  matches the existing occupancy count.
- Manual JS: hover filled / empty-no-gap / empty-gap; toggling off leaves the main
  heatmap intact.
- JSON size check at 30 s × 100 pps with the 500 ms refresh.
- `cargo test --all-targets`, `cargo clippy --all-targets` (`Memory.md` §8).

## 12. Implementation order

Phase 1: (1) subcarrier constant cleanup (Rust + web 64s) → (2) toggle + slot canvas
→ (3) slot tooltip with field meanings (placeholders) → (4) stats box (placeholders).
Phase 2: (5) `DiagnosticRecord` + Δ computation + serialization → (6) `--pps` on
`visualize` → (7) slot assignment/classification incl. window-end flag → (8) real
tooltip values → (9) per-second stats → (10) tests → (11) docs EN→VN → (12) README
row.
