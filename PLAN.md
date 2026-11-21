# plan.md — Opt-in diagnostic trace for live streaming

Status: implemented (reviewed — line references verified against HEAD `8c0d818`).

## Goal

The diagnostic trace (`VisualizationData.diagnostics`) is currently built
**unconditionally on every 500 ms streaming publish**, adding ~1 MB of JSON per
`/api/data` poll plus the classification CPU, even when the diagnostic toggle is
never used. Make it opt-in for live streaming, while keeping it always-on for
offline visualization of a collected `.bin`.

Note: the collection module itself never builds a payload — `collect` only writes
the raw `.bin`. "Offline" below means the `visualize` binary / `VisualizationData::from_file*`
on that file (one-shot).

## Current state (verified references)

Line numbers below were re-checked against the current working tree; the
earlier draft of this plan quoted pre-diagnostic-view numbers.

- `prepare_visualization_data_with_options` always builds the trace:
  `let diagnostics = Some(build_diagnostic_trace(...))` (`src/visualization.rs:1191-1197`).
- `VisualizationOptions` (`src/visualization.rs:1030-1043`) has only `nominal_pps`
  and `capture_end` — no on/off flag; `Default` at :1045-1052.
- Streaming publishes the sliding window every 500 ms via
  `prepare_visualization_data_with_options` (`src/streaming.rs:467-477`,
  `capture_end: false`), then `LiveDashboardSnapshot::Ready` → `GET /api/data`.
- Payload cost: ~1 MB of diagnostics at 34 s × 100 slots (`Memory.md:157`).
- Web already handles absence: toggle hidden when `diagnostics` is `None`
  (`web/visualizer.js:846-852` — `hasDiagnostics = !!(data && data.diagnostics)`,
  then `toggleBar.hidden = !hasDiagnostics` and `setEnabled(false)`); occupancy
  readout is browser-side from `timestamps` and unaffected.

## Behavior after the change

- Offline (`visualize`, `from_file*`, `prepare_visualization_data*`): unchanged —
  `diagnostics` always `Some(...)`.
- Streaming: `diagnostics` only when enabled. Default **off**; CLI `--diagnostics`
  turns it on. When off the payload carries `diagnostics: null` and the web UI
  hides the diagnostic toggle.

## Changes

1. `VisualizationOptions` (`src/visualization.rs:1030-1043`): add
   `pub diagnostics: bool`; `Default` (:1045-1052) keeps `true`. Gate the build at
   :1191: `let diagnostics = options.diagnostics.then(|| build_diagnostic_trace(...));`.
2. `StreamingConfig` (`src/streaming.rs:66-77`): add `pub diagnostics: bool`,
   default `false` in `Default` (:79-90).
3. `LiveAccumulator` (`src/streaming.rs:388-400`): add a `diagnostics: bool`
   field, initialized from `config.diagnostics` in `LiveAccumulator::new`
   (field list around :419). `LiveAccumulator` copies individual config fields
   rather than holding the `StreamingConfig`, so the flag must be threaded here
   explicitly. Pass it through at `streaming.rs:471-474`:
   `VisualizationOptions { nominal_pps: Some(self.nominal_pps), capture_end: false, diagnostics: self.diagnostics }`.
4. `VisualizationData::from_bytes_with_pps` (`src/visualization.rs:1243-1246`)
   constructs `VisualizationOptions` literally — set `diagnostics: true` so
   offline payloads keep the trace.
5. CLI (`src/bin/streaming.rs`): add a `--diagnostics` flag that sets
   `config.diagnostics = true`; document it in `USAGE` (:17-31). No CLI test
   (the existing `parse_args` tests stay as they are; the flag is a one-line
   assignment).
6. No web changes needed (`None` → toggle hidden is established behavior).
   The collection module is untouched.

## Tests

- One new test in `tests/visualization.rs`, reusing the existing
  `Sample`/`packet`/`prepare` helpers (:182-260): build a small synthetic packet
  set, assert that `prepare_visualization_data_with_options(..., diagnostics: false)`
  yields `diagnostics: None` (serialized as `"diagnostics":null`), while the
  default options keep `Some`.
- Decision: the older diagnostic tests that construct `VisualizationOptions`
  through the `prepare` helper (`tests/visualization.rs:258`) **may be removed**
  rather than migrated — they are not part of the accepted feature set and the
  single new test is the contract for this change.
- Verification: only the new test — `cargo test <new test name>`.

## Docs

- `docs_EN/streaming.md`: add `--diagnostics` to the CLI options table (:43-55),
  add `diagnostics: bool` to the `StreamingConfig` listing (:90-100), and note
  that the diagnostic toggle requires `--diagnostics` in live mode (occupancy
  unaffected). Mirror in `docs_VN/streaming.md` (:36-48, :99+).
- `docs_EN/visualization.md` §5: note that offline payloads always carry
  `diagnostics`; the ~1 MB cost note (:152-154) is now offline-only. Mirror in
  `docs_VN/visualization.md`.
- One line in `Memory.md` §2.6 after the cost line (:157): streaming defaults to
  no trace; `--diagnostics` enables it; saves ~1 MB per 500 ms poll.

## Implementation order

1. `VisualizationOptions.diagnostics` + gate at `visualization.rs:1191` +
   `diagnostics: true` at :1243
2. `StreamingConfig.diagnostics` + `LiveAccumulator.diagnostics` + pass-through
   at `streaming.rs:471` + `--diagnostics` in `src/bin/streaming.rs`
3. The single test → remove the tests the flag breaks → `cargo test <name>`
4. Docs EN → VN mirror → `Memory.md`
