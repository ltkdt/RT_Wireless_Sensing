# Plan — Capture-level Diagnostic Overview

## Goal

Add a capture-level overview to the diagnostic view: aggregate all empty slots of the entire capture and show *where the PPS went* — a donut chart of loss causes by percentage (RX device drop, desktop backpressure, TX/airtime loss, generator missed, ambiguous), plus KPI counters. The panel appears **before** the existing "Diagnostic slot grid" inside `#diagnosticSection`.

The Rust backend already computes everything (`empty_slots[].cause`, deltas, per-second stats), so this feature only **aggregates and renders** existing data.

## Current state (references)

- Backend: `DiagnosticTrace` at `src/visualization.rs:313-333` — `slots_per_second`, `origin_ticks`, `capture_end`, `records[]`, `empty_slots[]`, `seconds[]`; built in `build_diagnostic_trace` (`visualization.rs:521-599`) via `collect_empty_slots` (:602), `classify_empty_slot` (:645), `collect_second_stats` (:692).
- Frontend: `#diagnosticSection` at `web/index.html:103-118` (header → legend → slot canvas → details → per-second stats); `DiagnosticView` class at `web/visualizer.js:766+`, `CAUSE_LABELS`/`CAUSE_LAYERS` at `visualizer.js:722-738`; `render()` runs on every `/api/data` poll.

## Changes

### 1. Rust — `summary` block on `DiagnosticTrace`

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DiagnosticCauseCounts {
    pub device_drop: u64,
    pub desktop_backpressure: u64,
    pub air_loss: u64,
    pub generator_missed: u64,
    pub ambiguous: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticSummary {
    pub total_slots: u64,          // covered span: filled + empty (first→last record)
    pub filled_records: u64,
    pub empty_slots: u64,
    pub loss_ratio: f64,
    pub cause_counts: DiagnosticCauseCounts,
    pub dropped_frames_total: u64, // Σ (Δstream − 1) over empty slots
    pub delta_drop_total: u64,
    pub delta_backpressure_total: u64,
    pub empty_slots_low_rssi: u64,
}
```

- Add to `DiagnosticTrace`: `#[serde(default)] pub summary: DiagnosticSummary` — old payloads with `diagnostics` but no `summary` still parse; pre-feature payloads have `diagnostics: None` and keep the toggle hidden.
- New `collect_summary(&records, &empty_slots, slots)` called in `build_diagnostic_trace` after `collect_second_stats` (`visualization.rs:589`). Payload cost: ~10 numbers.

### 2. Web — overview panel above the slot grid

- `web/index.html`: inside `#diagnosticSection`, insert immediately after `.diagnostic-header` (:104-107) and **before** `#slotLegend`:
  ```html
  <div class="diag-overview" id="diagOverviewBox">
      <canvas id="overviewCanvas" width="200" height="200"></canvas>
      <div class="diag-overview-legend" id="overviewLegend"></div>
  </div>
  ```
  Retitle the section h2 to "Diagnostic view"; give the grid a subheading "Slot grid — selected second".
- `web/visualizer.js`: cache `overviewBox`/`overviewCanvas` in the `DiagnosticView` constructor; add `renderOverview()` called from `render()` — independent of the selected second, so it renders as soon as the toggle is on and refreshes each 500 ms poll (`setData` → `render`). Renders:
  - KPI row: records / total slots, loss %, empty slots, dropped frames, Δ`raw_drop_total`, Δbackpressure
  - donut of `cause_counts` (fixed color per cause), legend rows reusing `CAUSE_LABELS` + `CAUSE_LAYERS` + `%` per slice; `ambiguous` is its own slice so live-window edges don't distort attribution
  - fallback: if `summary` missing, aggregate `diagnostics.empty_slots` client-side (~10 lines)
- `web/style.css`: `.diag-overview`, KPI row, donut canvas, legend swatches (reuse `.legend-swatch`).

### Tooltip design for the capture-overview panel

**Markup** (`web/index.html`, inside the new `#diagOverviewBox`, which gets `position: relative`):

```html
<div class="diag-overview" id="diagOverviewBox">
    <div class="diag-overview-head">
        <h3>Capture overview</h3>
        <span class="diag-info" data-tip="summary" title="...">ⓘ</span>   <!-- explains the payload field -->
    </div>
    <div class="diag-kpis" id="overviewKpis"></div>       <!-- KPI rows carry data-tip -->
    <canvas id="overviewCanvas" width="200" height="200"></canvas>
    <div class="diag-overview-legend" id="overviewLegend"></div>  <!-- slice rows carry data-tip -->
    <div class="diag-tooltip" id="overviewTooltip"></div>
</div>
```

**JS** (`web/visualizer.js`): one `OVERVIEW_TIPS` map of one-line meanings (worded per the approved terminology in the terms file), rendered into `data-tip` attributes; a single delegated `mouseover`/`mouseout` listener on `#diagOverviewBox` shows/positions `#overviewTooltip` with the same edge-flip logic as `showTooltip` (:1143-1153). Snippet of the map:

```js
const OVERVIEW_TIPS = {
    summary:      'diagnostics.summary — capture-level aggregation of all empty slots between the first and last received record, precomputed in Rust',
    loss_ratio:   'empty slots / total covered slots over the whole capture',
    device_drop:  'Δstream gap + Δraw_drop_total > 0 — RX device dropped the record before sending (queue full / invalid / batch failed)',
    desktop_backpressure: 'Δstream gap + Δbackpressure > 0 — desktop client did not accept the batch; measured at the device socket, caused on the desktop',
    air_loss:     'contiguous counters + neighbouring RSSI ≤ LOW_RSSI_THRESHOLD_DBM (−70 dBm) — lost over the air, TX (AP) → RX',
    generator_missed:     'contiguous counters + normal RSSI — marker not sent / firmware produced no CSI',
    ambiguous:    'no boundary record closes the gap — capture end or live window edge',
    dropped_frames_total: 'Σ (Δstream − 1): packets offered to the stream but never received',
    delta_drop_total:     'total Δraw_drop_total — RX-side drops (records never left the device)',
    delta_backpressure_total: 'total Δraw_send_backpressure_total — batches the desktop did not accept'
};
```

Legend slices reuse `CAUSE_LABELS`/`CAUSE_LAYERS` (:722-738) as the title, the map entry as the meaning, and append the %.

**CSS** (`web/style.css`): add `.diag-tooltip` (clone of `.slot-tooltip` — absolute, `pointer-events: none`, amber border, `display:none`, `.tip-title` in amber) and `.diag-overview { position: relative; }` plus KPI-row styles. No changes to the existing `.heatmap-tooltip`/`.slot-tooltip`.


### 3. Test — exactly one new synthetic test

`capture_summary_aggregates_empty_slot_causes` in `tests/visualization.rs`, reusing the `Sample`/`packet`/`prepare` helpers (:182-252): one device-drop gap, one backpressure gap, one air-loss (contiguous counters + low neighboring RSSI), one trailing ambiguous slot. Assert `summary` cause counts, `dropped_frames_total`, `loss_ratio`, counter totals — and in the same test, that a `diagnostics` JSON without `summary` deserializes to `Default`. Existing 10 tests untouched.

Verification: only verify on the new test case, no need for testing the old tests.

### 4. Docs

- `docs_EN/visualization.md` §5: "Capture overview" subsection placed before the slot-grid part + `summary` row in the payload table (percentages are over empty slots within the first→last-record span; `capture_end` / `seconds[].partial` mark edges). Mirror in `docs_VN/visualization.md`.
- One invariant line in `Memory.md`: overview covers the covered span only; `ambiguous` includes window/capture edges.

## Implementation order

1. Rust struct + `collect_summary` + wiring
2. The single synthetic test → `cargo test capture_summary`, then full suite + clippy
3. Web: `index.html` panel → `visualizer.js` `renderOverview` → `style.css`
4. Docs EN → VN mirror → `Memory.md`