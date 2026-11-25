# plan.md — read-only traffic-mode handling, `--internal`, and the `device` binary

Status: Proposed
Replaces the previous `PLAN.md` (DSP view: resample + normalize), which is implemented and
recorded in `Memory.md` §2.7; it stays in git history.

## Goal

Two things, in this order of importance.

**1. The capture path stops writing to the device.** `collect` and `streaming` no longer send
`PATCH /sensing` in *either* mode. They read `GET /sensing` once, report what the device is
actually doing, and refuse to run when the device's mode contradicts the requested one. The
write moves to a new `device` module and a new `device` binary, where it is an explicit,
operator-initiated action instead of a side effect of recording.

**2. The traffic source becomes an explicit command-line choice.** `--internal` states that the
device produces its own traffic and that the client must not: no UDP marker task, no marker
port. Default (no flag) keeps today's external-marker behavior.

Scope is four binaries — `cargo run`, `cargo run --bin collect`, `cargo run --bin streaming`
(the three capture entry points, all gaining `--internal`) and the new `cargo run --bin device`
(the only writer) — plus the reader side they already share (`visualize` learns to read the
`_internal` suffix, so an offline capture is labelled instead of showing `—`), and the metadata
and labels they already share. `save` and `decode` are untouched, and the dashboard keeps its
two PPS figures: **nominal** (the device's own target rate in internal mode, with `--pps` as
the fallback) and **effective** (measured, never replaced).

Explicitly **not** in scope: any ESPectre firmware change. Exposing
`DELETE /sensing/overrides` and correcting the `261117` report's §3.2.3 are ESPectre-side work;
this repo only documents the shared contract (§10) and never waits on them.

Why read-only, and why the PATCH leaves the capture path: the device's own generator is
`ping`, the **factory default** (`@espectre docs/CHANGELOG.md:25`, `docs/SDK.md:274`), while
`external` means the device "sends nothing and listens for UDP markers". So the current
unconditional `PATCH {external}` is not "enabling sensing" — it is *taking ownership* of the
traffic, and because that setting is NVS-persisted it outlives the run: after the collector
exits, the AP is left in a mode where it generates no traffic at all. A capture tool should not
mutate the thing it measures. Internal mode needs no write whatsoever (it is the default), and
external mode needs none either — markers are counted by the device as inbound traffic either
way, and `GET /sensing` already tells us what the device is doing. Writing is therefore an
*operator* action, not a side effect of recording.

Why it matters now: the report `CSI/Report/261123_traffic_generator_mode_toggling.md` shows
that `traffic_generator_mode` is the one sensing setting with an **NVS store**
(`runtime_traffic_mode_store.cpp`, key `traffic_gen`), so it outlives a normal
`idf.py flash`. A collector that unconditionally patches `external` therefore
(a) silently overwrites the firmware's internal `ping` configuration, and
(b) sends UDP markers at 20 PPS to a device that is already generating its own traffic —
duplicate, uncounted stimuli in the same CSI capture.

Constraints: no new dependency; no change to the wire format or the decoder; two additive,
`#[serde(default)]` metadata fields (`traffic_mode`, `nominal_pps_source`); `external` stays the
default so existing commands keep working; a payload without the new fields renders exactly as
it does today.

## Decisions (settled)

| # | Topic | Decision |
|---|-------|----------|
| D1 | Flag shape | One boolean, `--internal`, on both `collect` and `streaming`. No `--external` counterpart: absence of the flag *is* external. |
| D2 | Default | `external` — identical to today's behavior. `--internal` is opt-in, so every existing script and doc example stays valid. |
| D3 | No `PATCH /sensing` in **either** mode | The capture path is **read-only** (settled). Internal mode never patches, and external mode no longer patches either — today's unconditional `PATCH {external}` (`collection.rs:307-318`) is removed. Reading is sufficient in both cases: `GET /sensing` reports `traffic_generator_mode` and `csi_target_pps` together (`@espectre docs/API.md:102-105`), so the collector learns what the device is doing without changing it. Nothing in the capture flow depends on the write: `GET /csi` streams regardless of mode. The write is not deleted, only **relocated** — `device::set_traffic_generator_mode()` (§1b) keeps it as an explicit operator operation. |
| D3b | Why the write was wrong, not merely unnecessary | The device's own generator is `ping`, the factory default (`@espectre docs/CHANGELOG.md:25`, `docs/SDK.md:274`), and `external` means the device "sends nothing and listens for UDP markers". The mode is NVS-persisted (`runtime_traffic_mode_store.cpp`, key `traffic_gen`, report §2), so the old PATCH left the AP generating **no traffic at all** after every capture, outliving the process. A measurement tool that mutates its subject is the failure blamed in report §4. Removing the write is therefore a bug fix, not a simplification. |
| D3c | Wrong device mode is fatal | Cleared by the same read that supplies the nominal. If the device's reported mode does not match the requested one, print one `error:` line naming the actual mode, the requested mode, and the one command that fixes it (§1b), then **exit non-zero before any bytes are captured** (settled: warn *and stop*). No capture file is created, so no misleading `_internal` name is ever written for an empty run. Trigger matrix: requested `internal` + device `external` → stop; requested `external` + device `ping`/`dns`/`dns_tcp`/`wifi_raw` → stop. |
| D4 | No markers, no tasks | Internal mode spawns no `UdpSocket`, no `AbortOnDrop` task, and resolves no marker target. Nothing to cancel, nothing to leak. |
| D5 | Config shape | `CollectionConfig.traffic_generator: TrafficGenerator` (`External` \| `Internal`), not a bare `bool`. The report lists more than two modes (`external`, `ping`, `wifi_raw`); a named enum leaves room without renaming a field later. |
| D6 | Summary | **`configured_pps` keeps its `u32` type** (settled). It documents the `--pps` value in force, which in internal mode is the *fallback* rate (D11) — true whether or not anything is driven. Who drives traffic is expressed by the new `traffic_mode: TrafficGenerator` field, not by nulling the rate. This avoids a public-API break and keeps `Memory.md` §2.6's `Option` convention for values that are genuinely unknown, not for values that are merely unused. `sensing: Option<String>` — `None` when no PATCH was made (the one field where "absent" is real). |
| D7 | Nominal PPS | `nominal_pps` reports the rate the capture is *expected* to hold, resolved on a four-rung ladder: (1) the device's own `csi_target_pps`, read with `GET /sensing` — **the primary source in internal mode**, because the device owns the rate there, and deliberately **not consulted in external mode**, where the laptop owns the rate and the argument is authoritative, (2) the `--pps N` argument — the **fallback**, used when the internal read fails or reports nothing usable; in external mode the argument *is* rung 1 by right, (3) the measured `effective_pps` (today's slot-grid fallback at `visualization.rs:1282`, unchanged), (4) the web constant `DEFAULT_NOMINAL_PPS = 20`. In internal mode rung 1 is not optional: the read always happens, pre-flight (D12b). |
| D8 | The mode is shown, and it carries the nominal's meaning | `metadata.traffic_mode: Option<TrafficGenerator>` (`"external"` \| `"internal"`, `#[serde(default)]`, `None` = unknown) is added beside `nominal_pps`, and the Packet Rate card gains a third line **Traffic: External / Internal** above Nominal/Effective. The mode tells the reader *what the nominal number means*, so `nominal_pps_source` shrinks to the one distinction the mode cannot express — "did the rate fall back to `--pps` because the device read failed" (§D8b). `effective_pps` is never replaced: always measured as `(frames − 1) / duration`, always displayed. |
| D8b | `nominal_pps_source` scope | Kept, with a single variant `Option<NominalPpsSource>` = `Device` \| `Argument` \| `None` (mode unknown). `Device` = the device's `csi_target_pps`, `Argument` = the `--pps` **fallback** fired. The card hint is then a pure function of `(traffic_mode, nominal_pps_source)`: internal + `Device` → "Device target PPS", internal + `Argument` → "`--pps` fallback (device rate unavailable)", external + `Argument` → "Requested `--pps` vs measured rate", external + `None` → today's text. |
| D9 | Startup delay | Kept in both modes. The 200 ms exists to let the device start sampling; it is not marker-specific. External behavior stays bit-identical. |
| D10 | Restoring the device | The collector never writes the mode, so there is nothing to "restore" — and that is the point. If a change is wanted, it is made deliberately before the run with `cargo run --bin device -- --mode …`, and confirmed by re-reading (`GET /sensing`), per the report's ranked methods. A collector that writes state on entry, or "restores" it on drop, is the exact failure mode blamed in that report's §4. |
| D11 | `--pps` and `--marker-port` under `--internal` | **`--pps` is the fallback, not an override** (settled). Precedence is the device's `csi_target_pps` first, then `--pps`. Rationale: in internal mode the device sets the rate, so a `--pps` that silently outranked it would be one more place where the client overrules the device — the failure this whole plan exists to remove. `--marker-port` is **rejected** (exit 2): nothing binds or sends to it. Neither is silently ignored. |
| D11b | Why `--pps` keeps an independent use | It is not only a nominal-rate hint. In `streaming`, `nominal_pps` also sizes the **Detector / occupancy slot grid** (click-to-compute, `web/visualizer.js:227`, `slotsPerSecond()` :964). Keeping `--pps` alive is what makes an internal-mode capture still usable with an *intended* rate when the device value is stale, and it is what makes the ladder rung 3 (measured) reachable at all. |
| D12 | When the device is queried | **Both** modes issue exactly one `GET /sensing`, always, **before** the accumulator is built and before `GET /csi`. The request count for an external run changes by exactly this one call (previously `GET /capabilities` + `PATCH /sensing`; now `GET /capabilities` + `GET /sensing`), so the HTTP *sequence* is the same length but no longer contains a write. |
| D12b | The read resolves before streaming starts | Settled. `LiveAccumulator::new` (`streaming.rs:266`) takes the accumulator by value into `run_decoder_loop`, and every completion branch (`:281`, `:290-306`, `:310-322`, `:323-334`) aborts `server_task` before the collection result is available — so a value that first appears in `CollectionSummary` (built at `collection.rs:342-350`, *after* `stream_to_file` returns) can never reach a live dashboard. The nominal is therefore a **pre-flight property of the run**, resolved by `streaming::run` *before it spawns anything*, and handed to `LiveAccumulator::new` as a plain argument. No `tokio::sync::watch`, no `oneshot`, no `set_nominal()`: there is no window in which the dashboard shows a substitute, so there is nothing for a channel to solve. |
| D13 | The `device` module owns all device state | New `pub mod device` (settled). It is the **only** module that speaks to the RX ESP32 about its mode, and it holds three functions: `read()` (`GET /sensing` → `DeviceStatus`), `set_traffic_generator_mode()` (the relocated PATCH, §1b), and `require_mode()` (the D3c check). `collection` and `streaming` call it; neither keeps its own HTTP copy of the logic. Keeps the device contract in one file that `docs/ESpectre_sdk.md` (§10) can describe exactly, and makes the operator-facing write explicitly opt-in rather than a side effect. |
| D14 | `GeneratorMissed` is **relabelled**, never folded | In internal mode there is no client-side generator, so the *name* is wrong — but the *evidence* is not: `GeneratorMissed` is the `delta_stream == 1` branch of `classify_empty_slot` (`:874-880`), which returns `dropped_frames = 0` and `delta_drop = 0`. Folding it into `DeviceDrop` was wrong: `DeviceDrop` requires `delta_stream > 1` **with** `delta_drop > 0` (`:860-866`), so a folded slot would be labelled a "drop" while carrying zero dropped frames and zero drop delta — self-contradicting the cause's own definition and the tooltip that reads it. Instead the **bucket keeps its identity and its evidence; only its display name and layer change**: internal mode renders it as *"no record generated — device generator or firmware"* (a label the device's own generator honestly satisfies) and maps it to `DiagnosticLayer::RxDevice` (the device produced no CSI), keeping `DiagnosticLayer::Generator` for **external** traces where a desktop generator really exists. Wire/serde names stay `generator_missed` / `generator`, so old payloads and `--json` exports are unaffected; `Ambiguous` and the five-bucket partition are untouched. |
| D15 | Capture provenance via filename suffix | `collect` in internal mode appends `_internal` before the extension when `--output` was left at its default (`capture_v8.bin` → `capture_v8_internal.bin`); an explicit `--output` is used verbatim, because a user-chosen path must not be rewritten. Nothing is written into the header — the wire format stays untouched. |

## Current state (verified)

**`src/collection.rs`**
- Module doc says the module "enables external-traffic sensing" and "feeds the device with UDP markers" — :4-6; same claim in the doc example — :17-27.
- `CollectionConfig { esp32_ip, api_port, marker_port, pps, origin, output, startup_delay, max_duration, verbose }` — :74-96, `Eq` derived, `Default` — :99-112 (`pps: DEFAULT_PPS` = 20, :61; `marker_port: DEFAULT_MARKER_PORT` = 5555, :58).
- Marker constants: `TRAFFIC_MARKER = F0 9F 91 BB` — :66-72.
- `base_url` :118-120, `marker_period` :123-125, `marker_target` :128-136, `validate` :139-145 (rejects `pps == 0`, parses the marker endpoint — both marker-only concerns).
- `CollectionSummary { output, bytes_written, duration, configured_pps: u32, capabilities, sensing: String }` — :148-164.
- `collect_with_sink` sequence :279-360: `GET /capabilities` :286, **`PATCH /sensing {"traffic_generator_mode":"external"}` :307-318** (removed entirely, D3), UDP task :320-339, `sleep(startup_delay)` :341-342, `stream_to_file` :344-345, `drop(marker_task)` :347. The PATCH is deleted and replaced by a `GET /sensing` read; the socket becomes conditional.
- No `GET /sensing` anywhere today: the module never reads device state, only `/capabilities` and the PATCH response (`sensing: String`, :174) — that string is a *write* acknowledgement and disappears with the write.
- Tests :394-475: `default_config_matches_the_project_defaults` :398, `marker_period_*` :433, `marker_target_*` :445, `zero_pps_is_rejected` :455, `max_duration_defaults_to_unlimited` :465.

**`src/streaming.rs`**
- `StreamingConfig { collection: CollectionConfig, port, warmup, update_interval, window, diagnostics }` — :68-92. No marker logic of its own: it clones `collection` into the collection task — :293-300.
- `validate()` :120-136 re-checks `collection.pps == 0` and would reject internal mode's `pps: 0`. `LiveAccumulator` — :385-437; `nominal_pps: f64` is built from `config.collection.pps as f64` (:392, :431) and fed into `VisualizationOptions { nominal_pps: Some(self.nominal_pps), … }` — :493-497. `StreamingSummary` — :139-152.

**PPS plumbing (what the UI reads today)**
- `VisualizationMetadata.nominal_pps: Option<f64>` — `src/visualization.rs:60-72`; written at :1296 from `options.nominal_pps` filtered to positive/finite (:1277-1279). **`slot_pps = nominal_pps.unwrap_or(effective_pps)`** — :1282; used for the diagnostic grid :1307 and the DSP grid :1340.
- So today: no `--pps` → `nominal_pps: null` → the web substitutes its own constant. The measured rate is *not* presented as nominal in the UI, only used internally for the grid.
- `web/visualizer.js`: `DEFAULT_NOMINAL_PPS = 20` :106; `getRequestedPps()` :184-194 (returns `null` when no argument); `getNominalPps()` :198-202 (argument else the constant); occupancy :227; Packet Rate card :304-321 (`metaNominalRate`, `metaNominalRateHint`, `metric-pair-default` for the fallback); `slotsPerSecond()` :964; diagnostic header :1623.

**`src/bin/collect.rs`**
- `USAGE` :20-36 (`--pps` :26, `--marker-port` :25), `parse_args` :101-152, `main` prints `Sensing:` and `"… at {} PPS"` from `configured_pps` :53-70, tests :191-240.

**`src/bin/streaming.rs`**
- `USAGE` :19-40, `parse_args` :95-146, `main` prints `bytes/records/window/duration` :52-70, tests :128-188.

**Docs / notes**
- `docs_EN/collection.md:10-21` (the 5-step sequence), `docs_EN/collection.md:35-36` (field table), `:131` (option list), `:141-142` (requirements). Mirror: `docs_VN/collection.md:14-15`, `:35-36`, `:130`, `:140`.
- `docs_EN/streaming.md:34-46` (CLI table), `:188` (Ctrl+C also stops the markers). Mirror: `docs_VN/streaming.md:40-41`, `:185`.
- `README.md:44`, `:59-60`, `:216`, `:231`. `Memory.md` §2.2 (:96-109) and §3 (:274+).

**Existing commands that must keep working unchanged:** `cargo run --bin collect`,
`cargo run --bin collect -- --pps 50 --quiet`, `cargo run --bin streaming -- --pps 100 --port 8080`.
Both must still succeed against a device that is already in `external` mode — but note they now
**stop** if it is not (D3c), which is the one intentional behaviour change to the default path.

## Behavior after the change

| Step | External (default) | `--internal` |
|------|--------------------|--------------|
| `GET /capabilities` | yes | yes |
| `PATCH /sensing {"traffic_generator_mode":"external"}` | **removed** (D3) | **removed** |
| `GET /sensing` (read `traffic_generator_mode` + `csi_target_pps`) | **yes**, always | **yes**, always |
| Device mode disagrees with the request | stderr `error:`, **exit non-zero**, nothing captured | stderr `error:`, **exit non-zero**, nothing captured |
| Resolved nominal (`csi_target_pps`) | not consulted (laptop owns the rate) | **the device value**, pre-flight |
| UDP marker task | `pps` markers/s to `ip:marker_port` | **none** |
| `--pps` on the CLI | drives the markers; omitted → `InvalidPps` | **fallback nominal** only, used when the device read fails; never drives anything |
| `--marker-port` on the CLI | accepted | **rejected, exit 2** |
| `startup_delay` | 200 ms | 200 ms |
| `GET /csi` → `.bin` | unchanged | unchanged, written to `*_internal.bin` when `--output` is omitted |
| `metadata.nominal_pps` | `Some(--pps)` else `null` → web shows 20 | `Some(csi_target_pps)`, else `Some(--pps)`, else `null` → web shows 20 |
| `metadata.traffic_mode` | `"external"` | `"internal"` |
| `metadata.nominal_pps_source` | `"argument"` or `null` | `"device"`, `"argument"` (fallback), or `null` |
| `metadata.effective_pps` | measured | measured (same formula) |
| Packet Rate card | **Traffic: External**, Nominal vs Effective, hint "Requested `--pps` vs measured rate" | **Traffic: Internal**, Nominal vs Effective, hint "Device target PPS" (or "`--pps` fallback" when the read failed) |
| Diagnostic `generator_missed` bucket | unchanged: same count, same evidence | **same count and evidence**, relabelled "device generator or firmware" and layered `rx` |
| `CollectionSummary.configured_pps` | `20` (unchanged type) | `20` / the `--pps` fallback value — always the rate in force |
| `CollectionSummary.traffic_mode` | `External` | `Internal` |
| `CollectionSummary.device_traffic_mode` | `None` | the device's reported mode, e.g. `Some("ping")` |
| streaming slot grid | `--pps` | device target PPS, else `--pps`, else measured |
| default main binary | unchanged | `--internal` accepted (same semantics) |

Offline, in the same table's terms (`cargo run --bin visualize -- FILE`):

| Input | `metadata.traffic_mode` | `metadata.nominal_pps` | `generator_missed` |
|---|---|---|---|
| `capture_v8.bin` | `null` → card shows `—` | `--pps` else `null` → 20 | present (five buckets) |
| `capture_v8_internal.bin` | `"internal"` (§7 suffix read-back) | `--pps` else `null` → 20 (the file cannot know the device rate) | **same count**, relabelled (§D14) — five buckets either way |

Because the card now names the mode, the ambiguity that motivated the whole nominal-PPS debate
disappears: a reader seeing "Traffic: Internal / Nominal 100" knows the 100 is the device's
own rate, and "Traffic: External / Nominal 20" knows the laptop drives it.

The capture bytes are identical in both modes; only the file name (D15), the card (D8) and the
diagnostic attribution (D14) differ. The file still cannot prove *where* the CSI came from
(report §3.2), which is why C4/D15 exist.

## Changes

### 1. `collection.rs`

1. **Mode enum — re-exported (P1 landed it in `device.rs`).** The enum lives in
   [`crate::device::TrafficGenerator`], so `collection.rs` gains
   `pub use crate::device::TrafficGenerator;` instead of declaring its own copy. Two copies
   would be a second source of truth for the same distinction, and `device` cannot depend on
   `collection` (that would form a cycle). Every call site in this plan
   (`CollectionConfig.traffic_generator`, `StreamingConfig`, the binaries) still writes
   `TrafficGenerator::Internal` exactly as specified — only the definition site moved.
2. **Config field.** `CollectionConfig.traffic_generator: TrafficGenerator` after
   `marker_port`; `Default` sets `External`. `marker_port`/`pps` keep their values so the
   struct stays total — internal mode just never reads them.
3. **Helpers.**
   - `pub fn markers_enabled(&self) -> bool` → `self.traffic_generator == External`.
   - `pub fn nominal_pps(&self) -> Option<f64>` → `(self.pps > 0).then_some(f64::from(self.pps))`;
     the single place a CLI `--pps` becomes a nominal rate, shared by both binaries.
   - `marker_period` / `marker_target` doc-notes: "marker mode only".
   - `validate()`: when internal, **skip** the `pps == 0` and endpoint checks (they are
     marker-only) and return `Ok(())`. The `marker_port` rejection for internal mode is a CLI
     concern (§3, §4), not a config error — `CollectionConfig` stays total. External path is
     unchanged.
4. **No device I/O here.** `collection.rs` stops performing the PATCH (:307-318) and never
   gains a `GET /sensing` of its own. All device-state access goes through the new `device`
   module (§1b, D13), whose `read()` result is passed in. `read_device_sensing` is therefore
   **not** added to `collection.rs` — that draft is superseded.
5. **Summary.** `traffic_mode: TrafficGenerator`, `device_traffic_mode: Option<String>`
   (what the device *reported*), `nominal_pps: Option<f64>` and
   `nominal_pps_source: Option<NominalPpsSource>` where
   `#[derive(Serialize)] pub enum NominalPpsSource { Device, Argument }` (serialized
   `"device"` / `"argument"`, `#[serde(default)]`). `configured_pps` **keeps `u32`** (§D6).
   **`sensing: String` is deleted** — it held the raw `PATCH` acknowledgement (":174") and there
   is no PATCH to acknowledge. `capabilities: String` stays. `device_traffic_mode` feeds
   `collect`'s stdout and the web payload's `traffic_mode`; it is not a separate web field.
   Every construction site (`collect_with_sink` :349-360) and every test is updated;
   `traffic_mode` is simply `config.traffic_generator`.
6. **Run sequence** in `collect_with_sink` (:279-360). The PATCH block is **deleted**, the
   read is a `device` call, and the read happens in **both** modes:
   ```rust
   // D13: the only place device state is read. Both modes, one request.
   let device = device::read(&client, &base, &config.origin).await;
   // D3c: refuse to capture in the wrong mode, before any file is created.
   device::require_mode(device.as_ref(), config.traffic_generator)?;
   let (nominal_pps, nominal_pps_source) = match config.traffic_generator {
       // Rung 1: the device owns the rate in internal mode.
       TrafficGenerator::Internal => match device.as_ref().and_then(|d| d.csi_target_pps) {
           Some(pps) => (Some(f64::from(pps)), Some(NominalPpsSource::Device)),
           // Rung 2: read failed / nothing usable -> the --pps fallback.
           None => {
               let fallback = config.nominal_pps();
               (fallback, fallback.map(|_| NominalPpsSource::Argument))
           }
       },
       // External: the argument, else nothing (web shows 20). The device rate is
       // deliberately not consulted — the laptop owns the rate here (D7).
       TrafficGenerator::External => match config.nominal_pps() {
           Some(pps) => (Some(pps), Some(NominalPpsSource::Argument)),
           None => (None, None),
       },
   };
   let marker_task = config.markers_enabled().then(|| { /* :320-339 verbatim */ });
   // … startup_delay, stream_to_file unchanged …
   drop(marker_task);
   ```
   `Option<AbortOnDrop>` keeps the cancellation guarantee (dropping an `Option` still drops
   the guard) and keeps the two paths identical. In internal mode the read happens even when
   `--pps` is given, because the device rate still outranks the flag (D11).
7. **Wrong mode stops the run (D3c).** `device::require_mode()` compares
   `device.traffic_generator_mode` with `config.traffic_generator`:
   - requested `Internal`, device `external` → `CollectionError::WrongDeviceMode { actual:
     "external", requested: Internal }`, message naming the fix:
     *"the device reports `traffic_generator_mode: external` but `--internal` was given; the
     device would generate no traffic. Set it to `ping` with `cargo run --bin device --
     --mode ping`, or drop `--internal`."*
   - requested `External`, device anything but `external` (`ping`, `dns`, `dns_tcp`,
     `wifi_raw`) → same error, mirrored, naming `--mode external`.
   - device mode **unknown** (read failed, or field absent) → **warning only**, run continues:
     a failed read must not make the tool unusable, and the ladder already has a documented
     fallback (D11). This is the one case where the run tolerates a mismatch.
   The check runs before `stream_to_file`, so a rejected run leaves **no capture file** and
   nothing to be mislabelled by the `_internal` suffix (D15).
8. **Module doc** :1-8 and the example :17-27: say "enables external-traffic sensing *unless*
   the configuration selects `Internal`". Add internal to the step list of `collect`'s doc
   :246-262, including the `GET /sensing` read.
9. **Output filename suffix (D15).** `CollectionConfig` gains
   `pub output_is_default: bool` (private-ish flag, `true` in `Default`, set to `false` by
   `--output` in every CLI, and by `with_output`). New helper
   `pub fn resolved_output(&self) -> PathBuf`: when internal **and** `output_is_default`,
   return the output path with `_internal` inserted before the extension
   (`capture_v8.bin` → `capture_v8_internal.bin`, no extension → `capture_v8_internal`);
   otherwise `self.output` unchanged. Every write site uses `resolved_output()`:
   `stream_to_file` (:392-393 is the only file create), `CollectionSummary.output`
   (:350-356), `StreamingSummary.output` via `build_summary` (`streaming.rs:370-380`), and
   `default_output_path()` stays as-is for compatibility. This keeps two consecutive runs
   (one per mode) from overwriting each other, which is the concrete cost of the missing
   on-wire provenance.
10. **Diagnostic relabel (D14).** `build_diagnostic_trace` (:1290-1310 call site) gains a
    `traffic_mode: Option<TrafficGenerator>` parameter (from `VisualizationOptions`) and passes
    it into the cause-to-layer mapping only — **`classify_empty_slot` (:851-882) is not
    touched**. The classification, the `dropped_frames`/`delta_drop`/`delta_backpressure`
    triple it returns, the five-bucket partition and every counter stay exactly as they are, so
    a `GeneratorMissed` slot still truthfully reports `dropped_frames = 0` and
    `delta_drop = 0`.
    - `DiagnosticCause::layer()` (:148-158) stays a pure `self -> layer` function for the
      external/unknown case; the mode-aware variant lives at the trace level as
      `fn layer_for(cause, mode)`: `GeneratorMissed` + `Some(Internal)` →
      `DiagnosticLayer::RxDevice`, everything else unchanged.
    - The **display name** is mode-aware too, but it lives in the **web layer**, not in Rust:
      `web/visualizer.js` already owns `CAUSE_LABELS` (:816) and `OVERVIEW_TIPS` (:845), so an
      internal trace renders *"no record generated — device generator or firmware"* while an
      external one keeps *"generator (desktop) or firmware"*. No new payload field is needed —
      `traffic_mode` is already there (§D8).
    - Serialized names are unchanged (`"generator_missed"`, `"generator"`), so old payloads,
      `--json` exports and any saved HTML keep parsing; the Rust enum keeps the variant, which
      also keeps it deserializable from payloads produced by older builds.
11. **Reverse the suffix (D15 read-back).** New free function
    `pub fn traffic_mode_from_filename(name: &str) -> Option<TrafficGenerator>`: returns
    `Some(Internal)` when the file stem ends with `_internal`, else `None` (unknown — *not*
    `Some(External)`, since a name proves nothing about an arbitrary file). It is the exact
    inverse of `resolved_output()`, and both live side by side so they cannot drift. Used by
    `visualize` (§11) and documented as "a hint, never a proof".

### 1b. New `src/device.rs` — the only module that owns RX-device state (D13)

A new top-level module, `pub mod device;` in `lib.rs`, holding **all** traffic-mode knowledge
about the RX ESP32. Rationale (settled): `collection` keeps capture, `streaming` keeps the
dashboard, and the device contract lives here — one file for `docs/ESpectre_sdk.md` (§10) to
describe, and one place where a future ESPectre endpoint change lands.

1. **`DeviceStatus`** — the parsed subset of `GET /sensing` this project cares about:
   ```rust
   #[derive(Debug, Clone, PartialEq, Eq)]
   pub struct DeviceStatus {
       /// `traffic_generator_mode`, e.g. "external" | "ping" | "dns" | "dns_tcp" | "wifi_raw".
       pub traffic_generator_mode: Option<String>,
       /// `csi_target_pps`, a plain integer in the JSON (default 100 on Native).
       pub csi_target_pps: Option<u32>,
   }
   impl DeviceStatus {
       /// True when the device generates its own traffic (anything but `external`).
       pub fn is_internal(&self) -> Option<bool>;
   }
   ```
   Field names verified against `@espectre src/cpp/frontend/native/espectre/native_direct_frontend.cpp:332-333`
   and documented at `docs/API.md:102-105`. Per `docs/API.md:91` clients must ignore unknown
   fields, so the parse is a `serde_json::Value` lookup of two top-level keys — not a strict
   struct deserialize, which would break when the device adds a field.
2. **`pub async fn read(client, base, origin) -> Option<DeviceStatus>`** — one
   `GET {base}/sensing` with the `Origin` header. Returns `None` on transport error, non-2xx,
   or unparseable body; a missing/non-positive `csi_target_pps` yields `None` *for that field
   only*, never a hard failure. Never writes. Called by `collection` (both modes, D12) and,
   internally, by `require_mode`.
3. **`pub fn require_mode(status: Option<&DeviceStatus>, requested: TrafficGenerator) ->
   Result<(), DeviceError>`** — implements D3c:
   - `status == None` → `Ok(())`, caller prints the unknown-mode warning;
   - `requested == Internal` and `is_internal() == Some(false)` → `Err(Mismatch{..})`;
   - `requested == External` and `is_internal() == Some(true)` → `Err(Mismatch{..})`;
   - otherwise `Ok(())`.
   The error carries `actual: Option<String>`, `requested: TrafficGenerator`, and a
   `fn remedy(&self) -> String` that prints the exact follow-up command (§1b.5), so the
   message and the fix cannot drift apart.
4. **`pub async fn set_traffic_generator_mode(client, base, origin, mode: &str) ->
   Result<DeviceStatus, DeviceError>`** — **the relocated PATCH** (`collection.rs:307-318`,
   moved verbatim in behaviour: same endpoint, same `Origin` header, same
   `{"traffic_generator_mode": mode}` body). It is *not* called anywhere in the capture path.
   It exists so the write is still possible, explicitly and on purpose, and so the only writer
   in the codebase is a module whose name says it touches the device.
5. **New binary `src/bin/device.rs`** — the operator-facing entry to item 4:
   ```
     --mode external|ping|dns|dns_tcp|wifi_raw   set the device's traffic generator mode
     --ip ADDR / --api-port PORT / --origin O    same defaults as collect
     --show                                      read and print the current mode, change nothing
   ```
   `--show` is the read-only form and is what the error messages tell the user to run first.
   Behaviour: print the mode before and after, then exit 0. Errors map to exit 1. This binary
   is the documented remedy for a D3c mismatch, which is why D3c can be fatal without being a
   dead end.
6. **Module doc** states the contract in the terms §10 will document: this project *reads*
   `GET /sensing` on every capture, *writes* only through `device`'s explicit operations, and
   never writes as a side effect of measuring.
7. **`Cargo.toml`**: no change. The crate uses the default `src/bin/*.rs` autodiscovery (the
   existing `collect`, `streaming`, `visualize`, `save`, `decode` binaries are not declared
   explicitly), so `src/bin/device.rs` is picked up automatically. No new dependency either:
   `reqwest` and `serde_json` are already used by `collection.rs` for exactly this kind of call.

### 2. `visualization.rs` (plumbing only — no new computation)

1. Two additive metadata fields next to `nominal_pps` (:68-73), both `#[serde(default)]` so old
   payloads parse and `visualize` payloads stay unchanged:
   - `traffic_mode: Option<TrafficGenerator>` — `"external"` \| `"internal"`, `None` = unknown
     (a payload built by `visualize`, which has no device).
   - `nominal_pps_source: Option<NominalPpsSource>` — `"argument"` \| `None` (§D8b).
   They are *display metadata*: the slot grid keeps
   `slot_pps = nominal_pps.unwrap_or(effective_pps)` (:1277-1282) exactly as today.
2. `VisualizationOptions.traffic_mode` / `.nominal_pps_source` (:1116-1151). Both are fieldless
   enums, so `VisualizationOptions` stays `Copy` and only the literals that care need updating
   (`streaming.rs:493-497`); `..Default::default()` literals are unaffected.
   `prepare_visualization_data_with_options` copies both into the metadata. With
   `traffic_mode: Internal` the resolved device rate arrives as `nominal_pps` from the
   collection summary (§3), never as a second, competing field.
3. No change to the reachable entry points' signatures: `from_bytes_with_pps` and friends keep
   `nominal_pps: Option<f64>` and pass `traffic_mode: None` / `nominal_pps_source: None`.
   `visualize` therefore renders exactly today's card, with no Traffic line, and keeps all five
   diagnostic buckets (D14).
4. `slot_pps` (:1282) stays `nominal_pps.unwrap_or(effective_pps)`; the trace call (:1307)
   gains `options.traffic_mode` so `layer_for` can relabel `GeneratorMissed` (D14) without
   touching `classify_empty_slot`. `prepare_visualization_data_with_options` passes it through
   to `build_diagnostic_trace`; the function is private, so no public signature churn.

### 3. `streaming.rs` — the nominal is resolved **pre-flight** (D12b)

The dashboard must show the device's `csi_target_pps` from its very first frame, and the
current code makes that impossible: `LiveAccumulator` is moved into `run_decoder_loop` at
spawn (`:266`, taken by value at `:516`), the value would only exist in `CollectionSummary`
(built at `collection.rs:342-350`, after the capture ends), and every completion branch aborts
`server_task` (`:283`, `:289`, `:298`, `:313`, `:327`, `:331`) *before* the collection result
is touched. So "learn it in `run()`'s completion branches" describes a path that does not
exist — a previous draft of this plan said exactly that, and it was wrong. The fix is to stop
treating the nominal as a late-arriving value.

1. **Pre-flight in `run()`**, between `config.validate()` and the first `tokio::spawn`:
   ```rust
   // D12b: resolve the nominal before anything is spawned, so the accumulator
   // receives a finished value and never publishes a substitute.
   let device = device::read(&client, &base, &config.collection.origin).await;
   device::require_mode(device.as_ref(), config.collection.traffic_generator)?;  // D3c
   let nominal = resolve_nominal(&device, &config.collection);   // the §1.6 ladder
   ```
   `resolve_nominal` is the same four-line ladder as §1.6, so it moves to a shared
   `pub fn` in `collection.rs` (or `device.rs`) and both call sites use it — one ladder, one
   place to change. This is the only reason `streaming` needs HTTP at all, and it is exactly
   one request, before anything is listening or spawning.
2. **`LiveAccumulator::new` gains three plain arguments**: `nominal: Option<f64>`,
   `source: Option<NominalPpsSource>`, `mode: TrafficGenerator`. The field `nominal_pps: f64`
   (:431) becomes `Option<f64>` and is set from the argument, **not** from
   `config.collection.pps`. No `watch`, no `oneshot`, no `set_nominal()`: with the value
   resolved at pre-flight there is no window to fill, so a channel would be dead weight.
3. `publish()` (:493-497) passes `nominal_pps`, `traffic_mode` and `nominal_pps_source`;
   `slot_pps` inside `prepare_visualization_data_with_options` falls back to `effective_pps`
   (unchanged).
4. Ordering constraint worth stating: the pre-flight read is a plain `.await` in `run()`, so it
   happens **before the `TcpListener` binds** only if it is placed first; place it after
   `validate()` and *before* `bind`, so a wrong-mode device fails before the port is taken and
   before any marker could be sent. This is the streaming analogue of `collection`'s
   "bind early so a busy port fails fast" comment (`:224-226`) — same principle, device first.
5. `validate()` (:120-136): keep the `pps > 0` check only when `markers_enabled()`; an internal
   `StreamingConfig` must validate with `pps: 0`.
6. Consequence for a live internal run without `--pps`: the card reads
   **Traffic: Internal / Nominal = device target / Effective = measured**, correct from the
   first published frame. With `--pps` the nominal is the argument only if the device read
   failed, and the hint says so.
7. Doc header :9-16: one sentence — the traffic source is chosen by
   `StreamingConfig::collection.traffic_generator`, the nominal rate is resolved pre-flight
   from the device, and a wrong-mode device aborts before the dashboard opens. No new field on
   `StreamingConfig`.

### 4. `web/` (label only — the slot-grid math is untouched)

1. `index.html` Packet Rate card (:40-51): one new `.metric-pair` row **Traffic** above Nominal,
   with `id="metaTrafficMode"`. No CSS addition needed — `metric-pair`, `metric-pair-label` and
   `metric-pair-value` already exist (`style.css:134-160`). Keep `metric-pair-strong` on
   Effective so the numbers still dominate, and consider the muted style for the mode.
2. `getRequestedPps()` (:184-194) returns the number only when
   `meta.nominal_pps_source === "argument"` (or the field is absent, for old payloads);
   `getNominalPps()` (:198-202) returns `nominal_pps` when it is a positive finite number, so a
   device-sourced nominal drives the occupancy grid with the device's own rate.
3. `applyMetadata` (:304-321): render the mode from `meta.traffic_mode` — `"External"` /
   `"Internal"` / `"—"` when null — and make the hint a pure function of
   `(traffic_mode, nominal_pps_source)`:

   | `traffic_mode` | `nominal_pps_source` | Traffic row | Nominal hint |
   |---|---|---|---|
   | `internal` | `"device"` | Internal | `Device target PPS vs measured rate` |
   | `internal` | `"argument"` | Internal | `--pps fallback (device rate unavailable)` |
   | `external` | `"argument"` | External | `Requested --pps vs measured rate` |
   | `external` | `null` | External | `No --pps argument — default 20 PPS` (muted) |
   | `null` (legacy, `visualize`) | any | `—` | today's text, unchanged |

   When the payload carries `diagnostics` **and** `traffic_mode === "internal"`, the
   `generator_missed` entry **stays visible** but is relabelled — D14 keeps the bucket and its
   evidence, only its name and layer change:

   | `traffic_mode` | `CAUSE_LABELS.generator_missed` | Layer shown |
   |---|---|---|
   | `"internal"` | `no record generated — device generator or firmware` | `rx` |
   | `"external"` / `null` | `no record generated — generator (desktop) or firmware` (today) | `generator` |

   Legend helpers affected: `CAUSE_LABELS` (:816), `CAUSE_LAYERS` (:824) and
   `OVERVIEW_TIPS.generator_missed` (:845). `CAUSE_COLORS` (:833) and the two hard-coded orders
   at :1094 and :1149 are untouched — nothing is filtered, so the donut keeps five slices and
   the panel keeps five rows in both modes.

4. `computeOccupancy` (:227), `slotsPerSecond` (:964) and the diagnostic panel (:1623) are
   otherwise untouched: they call `getNominalPps()` / read `diagnostics.slots_per_second`.

### 5. `src/bin/collect.rs`

- `USAGE` (:20-36): add, after `--pps`:
  ```
    --internal          do not drive the device: no UDP markers. Use when the ESP32
                        generates its own traffic (traffic_generator_mode = ping /
                        wifi_raw). The nominal rate comes from the device's
                        csi_target_pps; --pps is only the fallback when that read
                        fails. The device's mode is verified, never changed.
  ```
  Reword `--pps` and `--marker-port` with "(external mode drives the markers)". Add to the
  `--output` line: "… (default: capture_v8.bin, or capture_v8_internal.bin with `--internal`)".
  Add a final note: "The device's traffic_generator_mode is read, not written. To change it,
  use `cargo run --bin device -- --mode <mode>`."
- `parse_args` (:101-152): set `config.traffic_generator = TrafficGenerator::Internal` on
  `--internal`; afterwards, if internal and `--marker-port` was given, return
  `Err("--internal cannot be combined with --marker-port")`. Track presence with a local —
  the default value makes it indistinguishable otherwise. `--pps` passes through untouched.
  `--output` additionally sets `config.output_is_default = false`.
- `main` (:53-70): the `Sensing: …` line is **deleted** with the PATCH (§1.5). `configured_pps`
  stays a `u32`, so the existing `"… at {} PPS"` line keeps working unchanged; in internal mode
  it prints the rate in force (the `--pps` fallback value, or the default when none was given).
  Add one line showing what the device reported, so the read is visible even when the dashboard
  is not opened: `device       : traffic_generator_mode=ping, csi_target_pps=100` (or
  `device       : unreachable — mode not verified`). Then the resolved rate actually used for
  the slot grid: `rate         : 100 PPS (device target)` /
  `rate         : 50 PPS (--pps fallback)` / `rate         : 20 PPS (default)`.
- **Exit codes**: `0` success; `1` runtime/IO/HTTP failure; `2` argument error (including the
  `--marker-port` rejection); **`3` wrong device mode** (D3c), so a script can distinguish
  "misconfigured device" from "capture failed". The `error:` line goes to stderr and names the
  `device --mode …` remedy.

### 6. `src/bin/streaming.rs`

- `USAGE` (:19-40): same `--internal` text, plus "the nominal rate and the occupancy slot count
  come from the device's `csi_target_pps`; `--pps` is the fallback (and the only rate source in
  external mode). The device's mode is verified before the dashboard opens, never changed."
- `parse_args` (:95-146): same handling and the same `--marker-port` rejection message;
  `--output` sets `output_is_default = false`.
- `main` (:52-70): unchanged output; the `device`/`rate` lines and the D3c `error:` come from
  `run()`'s pre-flight (§3.1) on stderr, before the listener binds. Same exit code `3` for a
  wrong device mode.

### 6b. New `src/bin/device.rs` — explicit, opt-in mode changes (§1b.5)

- `USAGE`: `--show`, `--mode <external|ping|dns|dns_tcp|wifi_raw>`, `--ip`, `--api-port`,
  `--origin`, `--quiet`.
- `main`: `--show` → `device::read()` and print
  `traffic_generator_mode: ping (internal — the device generates its own traffic)` plus
  `csi_target_pps: 100`; exits `0`, writes nothing. `--mode M` →
  `device::set_traffic_generator_mode(M)`, then **re-read** and print the confirmed value
  (read-after-write, never trusting the PATCH response), exit `0`; `1` on failure. Neither
  flag → usage error, exit `2`.
- This binary is the documented remedy in every D3c message, so the error text and the fix
  ship together. It is the *only* executable that can change the device's mode.

### 7. `src/bin/visualize.rs` — read the suffix, never show `—`

`visualize` has no device to ask, so today an offline payload would show `Traffic: —`. The
`_internal` suffix written by `collect` (D15) is therefore read back, so an offline run shows
the same card a live run does instead of a placeholder:

- After `source_name` is computed (:97), call
  `traffic_mode_from_filename(&options.input.file_name()…)` (§1.11) and pass the result into
  `VisualizationOptions { traffic_mode, .. }` at the existing literal (:100-108). No CLI flag is
  added: the filename is the provenance, so asking the user to repeat it would duplicate state.
- Precedence with `--pps`: unchanged. `--pps` still sets `nominal_pps` (`Some(Argument)`),
  and when absent the nominal stays `None` → the web default 20, with the hint reading
  "`--pps` argument absent" rather than inventing a device rate that no live session produced.
  In other words the suffix restores **`traffic_mode`**, not `nominal_pps` — the offline file
  genuinely does not know the device's `csi_target_pps`.
- A file without the suffix behaves exactly as today (`traffic_mode: null`, `—`, five
  diagnostic buckets). A user who *renames* a capture can therefore lie about the mode; that is
  accepted and documented, because the alternative is no offline provenance at all.

### 8. `src/main.rs` (default binary)

- Parse only `--internal` (and `-h/--help`) from `env::args`, with no other options added:
  ```
  cargo run -- --internal
  ```
  An unknown argument is an error (exit 2) rather than silently ignored, since today the binary
  accepts nothing. `--internal` sets `CollectionConfig.traffic_generator`, and the summary print
  gains the same device/rate lines as §5. The stated scope becomes three binaries instead of
  two; `visualize`, `save` and `decode` remain untouched.
- Simple enough to keep `main.rs` a thin wrapper: no shared arg-parsing helper is introduced,
  because two tiny parsers are cheaper than a shared module for a 20-line flag set (revisit if
  `main` grows more flags).


### 9. Docs and notes

- `docs_EN/collection.md`: step list :10-21 (the sequence is now **five steps, no write**: read
  capabilities → read device state → verify the mode → optional markers → stream; steps 2-3 of
  the old list disappear and the old PATCH step is deleted, not skipped), field table :35-36
  (`traffic_generator`; `pps` = fallback nominal in internal mode; `output_is_default`, and
  `sensing` **removed** from `CollectionSummary`), a short **Traffic mode** section explaining
  the NVS caveat, the read-only rule, the `device` binary as the only writer, the wrong-mode
  abort with exit `3`, and the `_internal` suffix, pointing at the report; option list :131,
  requirements :141-142. Mirror in `docs_VN/collection.md`.
- **New `docs_EN/device.md`** (+ `docs_VN/device.md`): the `device` binary — `--show`,
  `--mode`, the read-after-write confirmation, why it exists (the capture path never writes),
  the NVS persistence caveat, and the exact commands §11 lists. This doc is the target of every
  D3c error message.
- `docs_EN/streaming.md`: CLI table :34-46, Ctrl+C note :188, one line on the nominal
  precedence (device → `--pps` → measured), and one line stating the rate is resolved
  **before** the dashboard opens (so the first frame is already correct). Mirror in
  `docs_VN/streaming.md`.
- `docs_EN/visualization.md`: `metadata.traffic_mode` and `metadata.nominal_pps_source` next to
  `nominal_pps` in the metadata section, with the Packet Rate card table from §4 item 3, plus a
  note that `cause_counts.generator_missed` is **relabelled, not removed**, for an internal-mode
  trace (D14: same count, same zero-delta evidence, different name and layer). Mirror in
  `docs_VN/visualization.md`. The diagnostic-view section (:57-171) names the five causes and
  their layers, so it needs the internal-mode label too.
- `README.md`: the **three-command mode-switching flow** (§11 below), the Traffic row in the
  dashboard description if one is listed, the `--internal` flag in the CLI summary, plus :44,
  :59-60 (EN) and :216, :231 (VN) to say the traffic source is a CLI choice and that the
  collector no longer changes the device mode.
- `Memory.md`: §2.2 (the run sequence, the now-read-only rule, and the `validate()` gate become
  mode-dependent), §2.6 (`traffic_mode` + `nominal_pps_source` join the PPS metadata contract;
  the `_internal` suffix rule), §3 (defaults table) — plus the header pointer on line 8.
  `Memory.md` §2.6's diagnostic bullet needs the D14 relabel, and a new bullet records the
  **pre-flight rule** (D12b) as a constraint future changes must respect: anything the live
  dashboard needs must be resolved before `run()` spawns, because the accumulator cannot be
  reached later.
- `docs_EN/collection.md` and `docs_VN/collection.md` gain the external link to the new
  `@espectre` `docs/ESpectre_sdk.md` (§10) next to the API-port/Origin conventions, so the
  borrowed-endpoint contract has one canonical explanation instead of two.

### 10. New ESPectre doc: `docs/ESpectre_sdk.md` (in the `@espectre` checkout)

Point 9 is **in scope**, and it takes the form of a new doc that explains the internal/external
distinction from the *device's* point of view and lists the API surface this project borrows.
It is a contribution to the ESPectre checkout (`~/Development/espectre/docs/`), not to this
repo; nothing in `rt-wireless-sensing` imports it, so the two stay loosely coupled.

**Placement.** `docs/SDK.md` already covers the field as a `RuntimeConfig` row (`:274`) and has
a short §Traffic destination (`:292-298`); `docs/CSI.md:9-24` covers the traffic sources from
the acquisition angle. Neither is the right home for a client-integration guide, and growing
`SDK.md` would blur its "configure before setup" focus. `ESpectre_sdk.md` is a **new standalone
page** that links to both rather than restating them, plus one added cross-reference line in
`SDK.md` §Traffic destination ("for a client-side view, see …").

**Content outline.**

1. **Two traffic models.** Internal = the RX device sends its own packets (`ping`, `dns`,
   `dns_tcp`, `wifi_raw`) and captures CSI on their responses; external = `EXTERNAL_HOST`
   sends nothing and listens on `csi_traffic_udp_port` (default `5555`) for a 4-byte UDP
   marker or unicast ICMP Echo Requests from another host. `csi_target_pps` sets the cadence in
   **both** cases and never enables or disables traffic (`SDK.md:270`, `CSI.md:9-11`).
2. **The one setting with an NVS store.** `persist_runtime_overrides` (`SDK.md:285`) makes
   `traffic_generator_mode`, `motion_on_hits`/`motion_off_hits` and the selectable detector
   survive reboots. A saved value outranks the Kconfig default on every boot
   (`esp_idf_runtime.cpp:136-155`), and `idf.py flash` does not erase the `nvs` partition —
   the whole reason the report `261123_traffic_generator_mode_toggling` exists. `--erase` is
   the only guaranteed reset.
3. **What is not persisted.** `traffic_generator_target_ip` (report §3.1), `csi_target_pps`,
   `csi_traffic_udp_port`, `csi_traffic_multicast_group`: these always follow the image.
4. **The API surface this project borrows.** A table of exactly the endpoints
   `rt-wireless-sensing` calls, with the source of truth for each:

   | Endpoint | This project uses it for | Contract |
   |---|---|---|
   | `GET /espectre/v1/capabilities` | reachability check before streaming (`collection.rs:286`) | `API.md:80-90` |
   | `PATCH /espectre/v1/sensing` | sets `traffic_generator_mode: "external"` (`collection.rs:307-318`) | `API.md:222-231` |
   | `GET /espectre/v1/sensing` | reads `csi_target_pps` and `traffic_generator_mode` (new, §1.4) | `API.md:94-107` |
   | `GET /espectre/v1/csi` | the raw stream written verbatim to `.bin` (`collection.rs:392`) | `API.md` CSI section |
   | UDP `:5555` marker `F0 9F 91 BB` | external traffic stimulus (`collection.rs:66-72`) | `CSI.md:9-24`, `API.md` external CSI traffic |

   Plus the two client conventions that cost a session to discover (report §3.3): port
   **62587**, and `Origin: https://test.espectre.dev` on **every** request, else `403 Origin
   required`.
5. **Who owns what.** The device owns the mode and the rate; the client owns the stimulus only
   in external mode. Explicit warning: a client that patches `external` unconditionally steals
   the mode from an internal device, and a client that sends markers to a `ping` device adds a
   second, uncounted stimulus in the same capture.

**Constraints for this doc.** Same style as the rest of `docs/` (link-rich, tables over prose,
no marketing). Every claim carries a source reference, the way `Memory.md` does. No claim that
this repo's `PLAN.md` cannot back with a citation — this doc must not become the origin of a
fact.

**Coordination.** The ESPectre repo is a separate working tree with its own review flow; the
doc is written there, not here. `rt-wireless-sensing`'s `docs_EN`/`docs_VN` link to it as an
external reference, the way `Memory.md:551-561` already treats ESPectre docs.

### 11. `README.md` — switching the device mode, in both directions

The report's top-ranked method is a Direct HTTP `PATCH`, so it belongs in the README where the
CLI options already are (EN after :59-60, VN after :231). Every request needs the `Origin`
header; the device must be on a network whose gateway answers ICMP for `ping` mode.

```bash
# ESPectre Direct HTTP API: port 62587, Origin header required (else 403).
DEV=10.178.22.9
H='-H Origin:https://test.espectre.dev'
J='-H Content-Type:application/json'
U=http://$DEV:62587/espectre/v1

# Always read the current mode first — it is NVS-persistent and outranks the
# build-time Kconfig default, so a reflash does not restore it.
curl -s $H $U/sensing

# Use the ESP32's own traffic generator  → run the collector with --internal
curl -s $H $J -X PATCH -d '{"traffic_generator_mode":"ping"}'     $U/sensing

# Back to laptop-driven traffic            → run the collector without --internal
curl -s $H $J -X PATCH -d '{"traffic_generator_mode":"external"}' $U/sensing

# Verify: the mode changed AND the stream still carries records
curl -s $H $U/sensing
curl -sN --max-time 6 $H $U/csi -o /tmp/csi_check.bin && xxd -l 6 /tmp/csi_check.bin
#   expect 4553 5052 01 08 3c 00  = "ESPR", v8, 60-byte prefix

# Escalation only: there is no API to clear the saved override. `--erase` wipes
# the whole nvs partition (Wi-Fi credentials included) and needs re-provisioning.
cd ~/Development/espectre
./espectre native flash --chip c5 --port /dev/ttyACM0 --erase
./espectre provision --chip c5 --port /dev/ttyACM0 --ssid <ssid>
```

README must state the pairing explicitly, because it is the whole point of the flag:

| Device mode | Collector command |
|---|---|
| `ping` / `wifi_raw` (device generates traffic) | `cargo run --bin collect -- --ip 10.178.22.9 --internal` |
| `external` (laptop drives traffic) | `cargo run --bin collect -- --ip 10.178.22.9 --pps 20` |

**The collector no longer changes the device mode**, so switching between the two rows is now a
separate, explicit step. README shows all three commands:

```bash
# Read the current mode (changes nothing):
cargo run --bin device -- --ip 10.178.22.9 --show

# Switch the device to its own generator, then capture:
cargo run --bin device -- --ip 10.178.22.9 --mode ping
cargo run --bin collect -- --ip 10.178.22.9 --internal

# Switch the device back to external host, then capture with markers:
cargo run --bin device -- --ip 10.178.22.9 --mode external
cargo run --bin collect -- --ip 10.178.22.9 --pps 20
```

A mismatch is now a **hard stop**, not a warning: `--internal` against an `external` device
prints `error: … the device would generate no traffic` on stderr, exits `3`, and writes no
file, so a wrong pairing can never produce a misleadingly-named capture. Likewise markers
against a `ping` device stop the run instead of adding a second, uncounted stimulus.

## Tests

**`collection.rs`**
- `traffic_generator_defaults_to_external` — `CollectionConfig::default().traffic_generator ==
  TrafficGenerator::External`, `markers_enabled()` is `true`, `output_is_default` is `true`.
- `internal_mode_validates_without_pps_or_target` — `pps: 0`, `esp32_ip: "not-an-ip"`,
  generator internal → `validate()` is `Ok`; the same config as external → `InvalidPps`.
- `nominal_pps_is_the_argument_or_nothing` — `nominal_pps()` is `Some(100.0)` for `pps: 100`
  and `None` for `pps: 0`, in both modes (the `--pps` → nominal mapping is mode-independent).
- `internal_mode_prefers_the_device_rate` — **the settled precedence**: with a device value of
  100 and `--pps 50`, the resolved nominal is **100** and the source is `Device`; with the
  device read `None` and `--pps 50`, it is `50` with source `Argument`; with neither, `None`.
  External mode never consults the device value, even if one is supplied.
- `resolved_output_appends_internal_suffix` — `(Internal, default)` → `capture_v8_internal.bin`;
  `(Internal, explicit)` → the explicit path unchanged; `(External, default)` →
  `capture_v8.bin`; an extension-less default → `capture_v8_internal`.
- `traffic_mode_from_filename_is_the_inverse` — round-trip property: for every case above,
  `traffic_mode_from_filename(resolved_output().file_name())` returns the same mode that
  produced the name. Negative cases: `capture_v8.bin` → `None`, `internal.bin` → `None` (no
  leading underscore), `capture_internal_v8.bin` → `None` (suffix must be terminal), and a
  non-UTF-8 / directories path is handled by the caller with `file_name().and_then(to_str)`.
- `device_sensing_parser` — **lives in `device.rs`, not `collection.rs`** (§1b.1). On a real
  `GET /sensing` body (copied into the test as a string literal, shape from `@espectre`
  `docs/API.md:94-107`):
  - `csi_target_pps: 100`, `traffic_generator_mode: "ping"` → `(Some(100), Some("ping"))`;
  - `csi_target_pps` missing → `(None, Some(mode))` — PPS misses, mode still parses;
  - `csi_target_pps: 0` or negative → `None`;
  - `csi_target_pps` as a JSON string or float → `None` (an integer field, not coerced);
  - unknown extra fields are ignored (`docs/API.md:91`: clients must ignore unknown fields).
  Parsing only: no network in tests.
- `internal_mode_warns_when_device_reports_external` — **replaced** by
  `wrong_device_mode_is_fatal` (D3c): `device::require_mode` is a pure function of
  `(Some(status), requested)` and asserts the full matrix —
  `(Internal, "external")` → `Err`,
  `(External, "ping")` → `Err`, `(External, "dns")` → `Err`,
  `(External, "wifi_raw")` → `Err`,
  `(Internal, "ping")` → `Ok`, `(External, "external")` → `Ok`,
  `(_, None)` → `Ok` (read failed: warn, do not block);
  and that `Mismatch::remedy()` names `--mode` with the right target for each direction.
- `collection_summary_has_no_sensing_field` — a compile-level assertion that
  `CollectionSummary` no longer carries `sensing` (§1.5), and that external construction
  therefore cannot claim a PATCH acknowledgement it never received.
- Summary construction: `configured_pps == DEFAULT_PPS` (a plain `20`, same type as today) /
  `traffic_mode == External` for external (with `device_traffic_mode == None` when the read
  failed, `Some("external")` when it succeeded); `configured_pps` still `20` or the `--pps`
  value for internal / `traffic_mode == Internal` / `Some("ping")`-style; and
  `CollectionSummary` has **no** `sensing` field in either mode (§1.5).

**`visualization.rs`**
- `traffic_mode_is_optional` — payloads prepared with `traffic_mode: None` (the `visualize`
  path) and with `Some(Internal)` produce **identical** `slot_pps`, amplitudes and timestamps;
  only the metadata field differs. Serialization round-trips, and an old payload without either
  field parses (`#[serde(default)]`).
- `internal_mode_shows_the_device_nominal` — with `traffic_mode: Some(Internal)` and
  `nominal_pps: Some(100.0)`, the metadata carries both, and `slot_pps` is 100 (not the
  measured rate), while `effective_pps` still reports the measured value.
- `internal_trace_keeps_the_generator_evidence` (D14) — on `capture_v8.bin` prepared with
  `traffic_mode: Some(Internal)` and with `None`:
  - `cause_counts` are **identical** in both traces, bucket for bucket (nothing is folded);
  - `generator_missed` slots still carry `dropped_frames == 0` and `delta_drop == 0`, i.e. the
    evidence keeps contradicting `DeviceDrop` — the assertion that locks this decision;
  - the five buckets sum to `empty_slots` in both;
  - `layer_for(GeneratorMissed, Some(Internal)) == RxDevice` while
    `layer_for(GeneratorMissed, None) == Generator`, and `layer_for` is the identity for the
    other four causes in every mode.
  Both traces are also asserted against the current golden numbers
  (`tests/visualization.rs:680`), proving the classification path is untouched.

**`streaming.rs`**
- `internal_streaming_config_validates_with_zero_pps` — `pps: 0` + internal → `Ok`.
- `nominal_pps_is_a_constructor_argument` — a `LiveAccumulator` built with
  `nominal: Some(100.0)` publishes `metadata.nominal_pps == Some(100.0)` and
  `metadata.traffic_mode == Some(Internal)` **on its first `publish()`**, with no prior tick
  and no setter called; built with `None` it publishes `None` and the slot grid falls back to
  `effective_pps`. The assertion is on the first published snapshot specifically, because that
  is what the old summary-handoff draft could not satisfy (D12b).
- `resolved_nominal_matches_the_collection_ladder` — `resolve_nominal()` is shared by
  `collection` and `streaming`, so one table-driven test asserts both call sites agree on
  `(mode, device value, --pps)` → `(nominal, source)` for all six combinations.

**`src/bin/collect.rs`** and **`src/bin/streaming.rs`** and **`src/main.rs`**
- `internal_flag_selects_internal_mode` (all three binaries).
- `pps_is_accepted_with_internal` — `--internal --pps 100` parses, and the resulting config has
  `pps: 100` **and** `traffic_generator: Internal`; the flag is a *fallback*, so the test also
  asserts it does not disable or reject anything.
- `internal_rejects_marker_port` — `--internal --marker-port 5555`, in both argument orders,
  returns `Err`; plain `--internal` and `--internal --pps 100` do not.
- `output_flag_clears_the_default_marker` — `--output x.bin` yields `output_is_default == false`;
  no `--output` keeps it `true`, so the `_internal` suffix applies only to the default.
- `main_accepts_only_internal` — `src/main.rs`: `--internal` parses, `--help` prints, and any
  other argument is an error (exit 2).
- `defaults_match_the_library_defaults` (:199 in each) still passes unchanged.

**`src/bin/device.rs`**
- `show_and_mode_are_exclusive_or_required` — neither flag → exit `2`; `--show --mode ping` →
  exit `2`; each alone parses.
- `mode_rejects_unknown_values` — `--mode pacing` (a real ESPectre runtime value that is *not*
  a valid `traffic_generator_mode`) exits `2` rather than sending it. Mirrors the firmware's own
  rejection in `@espectre test/cpp/suites/frontend/test_native_mqtt_frontend.cpp:293-294`.
- `show_issues_no_write` — asserted at the request level: the `--show` path builds a client and
  calls only `device::read()`, so any `set_traffic_generator_mode` call is a construction-time
  impossibility rather than a runtime check.

**`src/bin/visualize.rs`**
- `suffix_sets_the_traffic_mode` — a capture named `*_internal.bin` produces a payload with
  `metadata.traffic_mode == Some("internal")`; `capture_v8.bin` produces `None` (unchanged
  behaviour).
- `suffix_does_not_invent_a_nominal_rate` — the same two inputs with no `--pps` both yield
  `metadata.nominal_pps == None` and `nominal_pps_source == None`, proving the suffix restores
  only the mode; with `--pps 20` both yield `Some(20.0)` + `"argument"`.
- `suffixed_capture_relabels_the_generator_bucket` — an `*_internal.bin` input yields the same
  `cause_counts` as the unsuffixed copy, with `traffic_mode == Some("internal")` so the web
  layer can relabel (D14 via §7).

**`web/` (manual checklist, no JS harness in the repo)**
- **Traffic line** renders `External`, `Internal` or `—`, and matches the payload field in all
  five rows of the §4 item 3 table.
- `internal` + no `--pps` → "Device target PPS vs measured rate", and the occupancy click
  reports `N = device target` slots.
- `internal` + `--pps` → "--pps fallback (device rate unavailable)".
- `internal` → diagnostic panel keeps **five** entries; only the `generator_missed` label and
  layer change ("device generator or firmware", layer `rx`). `external` → five entries with
  today's wording ("generator (desktop) or firmware", layer `generator`).
- `external` unchanged in both cases; `traffic_mode: null` (old payload, `visualize`) shows
  `—` and today's hint, with no layout jump.
- `effective_pps` row identical in all cases.

**Not unit-tested (documented instead):** that a capture sends *no* `PATCH` and internal mode
sends *no* UDP packet, and that the device read actually returns the device's numbers, all need
a real HTTP endpoint. Verification is manual and explicit:

```bash
# Neither the PATCH nor the markers may appear: the device keeps its own mode.
H='-H Origin:https://test.espectre.dev'
U=http://10.178.22.9:62587/espectre/v1
curl -s $H $U/sensing    # before: note traffic_generator_mode and csi_target_pps
cargo run --bin collect -- --ip 10.178.22.9 --output internal_v8.bin --duration 10 --internal
curl -s $H $U/sensing    # after: BOTH fields unchanged, because nothing was written
```

What the run must show: `device : traffic_generator_mode=ping, csi_target_pps=100`, and a
dashboard whose Nominal equals **that** `csi_target_pps` — not the measured rate, not 20. A
second run with `--pps 50` must **still** show 100 (the flag is the fallback, not an override).
A third run against a device whose `/sensing` is unreachable must fall back to `--pps`/20,
print the "mode not verified" warning, and continue. A fourth run against a device set to
`external` must print the D3c `error:`, **exit 3, and create no file**. Every internal run
without `--output` must land in `capture_v8_internal.bin`.

The mode-switching half of the loop is exercised with the new binary, and it is the only part
that may change the device:

```bash
cargo run --bin device -- --ip 10.178.22.9 --show           # read-only, exits 0
cargo run --bin device -- --ip 10.178.22.9 --mode external  # the only writer
cargo run --bin device -- --ip 10.178.22.9 --show           # confirms, read-after-write
```

A fifth run after `--mode external` must then succeed as an external capture, proving the two
halves fit together.

No test may read `capture_v8.bin` for this feature; the existing files are unaffected.

## Docs (acceptance checklist)

- [x] `cargo test --all-targets` and `cargo clippy --all-targets` clean.
- [ ] `--internal` appears in `collect --help`, `streaming --help` **and** `cargo run -- --help`.
- [ ] **No binary sends `PATCH /sensing` during a capture.** Verified for both modes, by device
      log or proxy: `collect` and `streaming` issue `GET /capabilities`, `GET /sensing`,
      `GET /csi` — and nothing else. Only `cargo run --bin device -- --mode …` writes.
- [ ] `cargo run --bin device -- --show` prints the device's real mode and changes nothing;
      `--mode ping` then `--show` reflects the change (read-after-write confirmed).
- [ ] `collect --internal --pps 20` runs; `collect --internal --marker-port 5555` exits 2 with
      the documented message.
- [ ] `GET /sensing` before and after an internal run is identical, and the printed
      `csi_target_pps` matches `metadata.nominal_pps` for a run without `--pps`.
- [ ] An external run's capture is byte-identical to a run of the pre-change binary on the same
      device and network, and it issues **no PATCH** (one extra `GET /sensing` in its place).
- [ ] An internal run with `--pps` still uses the device rate (fallback semantics, D11).
- [ ] Default output in internal mode is `capture_v8_internal.bin`; an explicit `--output` is
      honoured verbatim.
- [ ] **A deliberately mismatched run stops**: `--internal` against an `external` device, and
      markers against a `ping` device, each print one `error:` line naming the `device --mode …`
      remedy, exit `3`, and leave **no capture file** anywhere.
- [ ] An unreadable device (wrong IP / device down) only *warns* about the unverified mode and
      continues — the copy-the-file-and-analyze-offline path is not blocked.
- [ ] Dashboard Packet Rate card shows the Traffic row (`External` / `Internal` / `—`) and the
      right hint for every combination in the §4 table, with `effective_pps` unchanged.
- [ ] In a live `streaming --internal` run the Nominal figure is the device's
      `csi_target_pps` **from the first published frame** — never 20 followed by 100 (D12b).
- [ ] Diagnostic panel keeps five entries in both modes; in internal mode the `generator_missed`
      row reads "device generator or firmware" with layer `rx`.
- [ ] Legacy payloads (`visualize` of a file without the suffix, or a payload without
      `traffic_mode`) render the current card with `—` and no layout jump, while
      `visualize capture_v8_internal.bin` shows `Traffic: Internal` and the relabelled
      `generator_missed` entry.
- [x] `README.md` carries the three-command switching flow (`--show`, `--mode ping` + `--internal`,
      `--mode external` + `--pps 20`) plus the mode ↔ flag pairing table.
- [x] `docs_EN` and `docs_VN` both describe the mode, the read-only capture path and the `device`
      binary, the `--duration` deadline and the `visualize --duration` slot span.
- [x] `docs_EN/device.md` and `docs_VN/device.md` document the module itself, linked from the
      module index in both languages.
- [ ] `@espectre` `docs/ESpectre_sdk.md` exists (§10), and every claim in it cites a source
      (source line, `API.md`/`SDK.md`/`CSI.md` section, or this report).

## Implementation order

Seven phases. Each ends with something that can be run or read, and phases are
**independently landable** — P1–P3 change no command-line behaviour, P4 is the only phase that
alters what an existing command does, and P5–P7 are additive. A phase may be reverted without
reverting the ones before it.

Progress: **P1 ✅ done · P2 ✅ done · P3 ✅ done · P4 ✅ done · P5 ✅ done · P7 ✅ done ·
P8 ✅ done** · P6 pending (needs the device).

### P1 — `device` module (no behaviour change to any binary) — ✅ **IMPLEMENTED**

The foundation, and the only phase that adds a module. Pure addition: nothing calls it yet.
1. `src/device.rs` with `DeviceStatus`, `is_internal()`, `read()`, `require_mode()`,
   `set_traffic_generator_mode()`, `DeviceError`/`Mismatch` + `remedy()` (§1b.1-1b.4).
2. `pub mod device;` in `lib.rs`; module doc stating the read/write contract (§1b.6).
3. Tests: `device_sensing_parser`, `wrong_device_mode_is_fatal` (full matrix),
   `remedy_names_the_right_mode` — all network-free (§Tests).

**Status: done.** `src/device.rs` (≈600 lines incl. tests) + one line in `lib.rs`.
14 module tests pass, `cargo clippy --all-targets` reports 0 warnings, `cargo build` clean.
Actual API delivered:

| Item | Note |
|---|---|
| `TrafficGenerator { External, Internal }` | defined **here**, not in `collection.rs` — P3 re-exports it so `collection` and `device` share one type without a cycle. `Default = External`. |
| `DeviceStatus { traffic_generator_mode, csi_target_pps }` | both `Option`; `is_internal()`, `traffic_generator()`. |
| `parse_status(&str) -> Option<DeviceStatus>` | split out of `read()` so parsing is unit-testable with no network. |
| `read(&DeviceEndpoint) -> Option<DeviceStatus>` | one `GET /sensing`; `None` on any failure. |
| `require_mode(Option<&DeviceStatus>, TrafficGenerator)` | the D3c matrix; `Ok` on unreadable/unknown. |
| `set_traffic_generator_mode(&DeviceEndpoint, &str)` | the only write; PATCH then **re-reads**. |
| `DeviceError::{Http, Mismatch, UnknownMode}` | `UnknownMode` replaces the plan's implied exit-2 path inside the library, so a typo is rejected before it can reach NVS. |
| `Mismatch::remedy(host, api_port)` | renders the copy-pasteable `device --mode …` command. |

Two deviations from the plan text, both recorded here:
- `TrafficGenerator` lives in `device.rs` (plan §1.1 placed it in `collection.rs`). Keeping it
  in `device` avoids `collection` → `device` → `collection`; P3 adds
  `pub use crate::device::TrafficGenerator;` to `collection` so every call site in the plan
  (`CollectionConfig.traffic_generator`, etc.) still compiles as written.
- `parse_status` was added as a separate function. The plan assumed parsing inside `read()`;
  splitting it is what makes the six parser tests possible without a mock HTTP server.

- **Exit criterion met:** `cargo test` green for the new module; `cargo build` produces a
  library with no new public behaviour. No binary changed.
- **Test-suite repair that came with P1 (prerequisite, not part of the feature).** Every
  fixture-dependent test pointed at `capture_v8.bin` in the repository root, a file that no
  longer exists there: commit `d5a6bdd` moved the sample to `samples/` and left the legacy
  691-record capture behind in the gitignored `artifacts/`. On a fresh clone **all** of
  `tests/capture_v8.rs` (6), `tests/saving.rs` (1), `tests/visualization.rs` (4) and
  `visualization`'s unit tests (3) failed, for 14 failures total — not the 3 first thought.
  Fixed by rebasing the whole suite onto the committed `samples/capture_v8.bin`:
  - new `src/fixtures.rs` resolves `samples/capture_v8.bin` from `CARGO_MANIFEST_DIR`, so the
    path lives in one place and works for lib tests and integration tests alike (a
    `tests/common` helper cannot be `use`d from `src/`);
  - golden numbers rebased to the real fixture: 501 records, 4.999 419 s, 100.01 PPS, payload
    hash `0xe89ed88960fad0b4`, session `fbda2f63…`, device `0xe3ec7259d2c354a3`, first record
    ticks 29 512 824 µs, last 34 512 243 µs, I/Q sum −493;
  - two assertions re-expressed instead of re-tuned, because their old constants encoded the
    legacy capture's 34.5 s length: the DSP grid test now derives its expected column count
    from `duration × slot rate`, and the `--pps` grid test asks for 200 PPS (not 100) so the
    configured grid is provably not the measured one.
  This is a test-only change with no effect on shipped behaviour, and it is what makes
  `cargo test --all-targets` meaningful again: **169 passed, 0 failed**, clippy clean.

### P2 — `device` binary (the explicit writer) — ✅ **IMPLEMENTED**

1. `src/bin/device.rs` with `--show` / `--mode` / `--ip` / `--api-port` / `--origin` (§6b).
2. Read-after-write: `--mode` re-reads and prints the confirmed value, never trusting the PATCH
   response.
3. Tests: argument parsing, exit codes (`0`/`1`/`2`), `--show` alone performs no PATCH
   (asserted by the absence of any request beyond `GET /sensing`).

**Status: done.** `src/bin/device.rs` + 4 new library tests in `device.rs`.
`cargo test --all-targets`: **185 passed, 0 failed**; `cargo clippy --all-targets`: 0 warnings;
`cargo fmt --check` clean. Built and exercised against the local stub below.

Delivered CLI (as specified, plus `--quiet`):

```
device (--show | --mode MODE) [OPTIONS]
  --show            print the current mode and rate, changing nothing
  --mode MODE       ping | dns | dns_tcp | wifi_raw | external
  --ip / --api-port / --origin / --quiet / -h, --help
```

Observed exit codes, verified by running the binary:

| Invocation | Exit | Output |
|---|---|---|
| `--help` | 0 | usage |
| (no action) | 2 | `error: one of --show or --mode is required` |
| `--show --mode ping` | 2 | `error: --show and --mode are mutually exclusive` |
| `--mode pacing` | 2 | `--mode must be one of: …` — rejected before any request |
| `--show` against a down device | 1 | `could not read …/sensing; is the device reachable?` |
| `--mode ping` against a down device | 1 | the PATCH error, not a `GET` error |

One design point behind the mutual exclusion: `--show` and `--mode` are different operations, so
requiring exactly one means a mistyped invocation can never *write* when the user meant to look.

### P2 — extra library work found while implementing

Two defects in P1's `set_traffic_generator_mode` surfaced once the binary exercised it, both
fixed here:

1. **`unwap_or_default()` hid an unknown outcome.** A failed post-write re-read returned
   `DeviceStatus::default()` (all `None`), which the binary then reported as *"the change did not
   take effect"* — a false claim, since the change may well have applied. Now
   `DeviceError::Unconfirmed { mode }`, whose message says the write was accepted but could not
   be confirmed and points at `--show`. Locked by `unconfirmed_is_distinct_from_a_write_failure`.
2. **The write path reported the wrong operation.** A connection failure during `--mode` printed a
   `GET /sensing` URL. The PATCH now fails as itself; only the confirming read can produce
   `Unconfirmed`.

Three **real-socket** tests were added to `device.rs` (`read_returns_the_parsed_status`,
`read_is_none_on_a_non_success_status`, `set_patches_then_re_reads`,
`set_rejects_an_unknown_mode_without_sending_anything`), using a `TcpListener` on port 0 with the
accept inside the server thread. They assert the actual wire behaviour the plan cares about:
`--show` issues **one** `GET`, and `--mode` issues a `PATCH` followed by a `GET`, in that order.

- **Exit criterion met:** `cargo run --bin device -- --ip … --show` prints the device's real mode
  against a live device; the equivalent path is covered by the socket tests in CI. Read-only by
  default.

### P3 — Library plumbing (still no behaviour change) — ✅ **IMPLEMENTED**

Pure refactor of the capture path, with the old write removed but no new flag yet.
1. `collection.rs`: `TrafficGenerator`, config field, `markers_enabled()`, `nominal_pps()`,
   `validate()`, `resolved_output()` + `output_is_default` (§1.1-1.3, §1.9),
   `traffic_mode_from_filename()` (§1.11).
2. `collection.rs`: **delete the PATCH** (§1.6) and delete `CollectionSummary.sensing`
   (§1.5); add `traffic_mode`, `device_traffic_mode`, `nominal_pps`, `nominal_pps_source`;
   call `device::read()` + `device::require_mode()`; extract the shared `resolve_nominal()`.
3. `visualization.rs`: two metadata fields + the `layer_for` relabel (§2).
4. `streaming.rs`: `validate()` gate, **pre-flight resolution in `run()`**, accumulator
   arguments (§3).
5. Tests: config defaults, the nominal ladder (both modes), suffix round-trip, summary shape.
- **Exit criterion:** `cargo test` green. `cargo run --bin collect -- --pps 20` against an
  `external` device still captures, and now issues one `GET /sensing` instead of a PATCH. This
  is the phase where the write disappears — it should be reviewed as its own change.
- **Note:** `streaming` starts issuing HTTP in this phase (§3.1). That is the one boundary
  crossing in the whole plan and deserves a reviewer's attention on its own.

**Status: done.** `cargo test --all-targets`: **198 passed, 0 failed** (was 185);
`cargo clippy --all-targets`: 0 warnings; the doc tests (32) pass too. Formatted files that
this phase touched; `src/bin/visualize.rs` is left as-is because it was already not
rustfmt-clean before P3 (verified by `rustfmt --check` against the committed revision), and a
crate-wide `cargo fmt` would rewrite unrelated files.

**The write is gone from the capture path, verified structurally rather than by inspection:**
the whole crate now contains exactly **one** `.patch(` call — `device.rs:411`, inside
`set_traffic_generator_mode` — and the only caller of that function is `src/bin/device.rs`.
`collection` and `streaming` call `device::read` and nothing else. That is D3 as a greppable
invariant, not a promise.

Delivered API:

| Item | Note |
|---|---|
| `pub use crate::device::TrafficGenerator;` | one definition, re-exported (§1.1). Both enums now derive `Serialize`/`Deserialize` `lowercase` so the mode crosses into payloads as `"internal"`/`"external"`. |
| `CollectionConfig.traffic_generator` | defaults to `External`, so **P3 changes no command's behaviour**. |
| `CollectionConfig.output_is_default` | `true` in `Default`, cleared by `with_output`; P4's CLIs clear it when `--output` is given. |
| `markers_enabled()` / `nominal_pps()` | `nominal_pps` is the only place `--pps` becomes a nominal rate. |
| `validate()` | marker-only checks gated on `markers_enabled()`, so an internal run validates with `pps: 0` and a hostname-shaped IP. |
| `resolved_output()` + `traffic_mode_from_filename()` | the `_internal` suffix and its exact inverse, side by side so they cannot drift. |
| `resolve_nominal(&self, Option<&DeviceStatus>)` | the §1.6 ladder **as a method on the config**, not a free function — D11's precedence is a property of the configuration, and putting it here means both capture paths necessarily share it. |
| `NominalPpsSource { Device, Argument }` | serializes as `"device"`/`"argument"`. |
| `CollectionSummary` | `sensing` **removed**; `traffic_mode`, `device_traffic_mode: Option<String>`, `nominal_pps`, `nominal_pps_source` added; `configured_pps` keeps `u32` (D6). |
| `CollectionSummary::device_line()` / `rate_line()` | one shared renderer, so `collect`, `streaming` and `main` cannot print the same fact three ways. `device_line()` is also where the "unknown (GET /sensing failed)" wording lives. |
| `CollectionError::WrongDeviceMode` | plus `CollectionError::Device` as the catch-all; `From<device::DeviceError>` maps `Mismatch` to the specific variant. |
| `StreamingError::Device` | the pre-flight read is the one new failure mode `streaming` can hit before it binds. |
| `VisualizationMetadata.traffic_mode` / `.nominal_pps_source` | both `#[serde(default)]`, both `None` for `visualize` payloads without the suffix. |
| `VisualizationOptions.traffic_mode` / `.nominal_pps_source` | fieldless `Copy` enums, so `..Default::default()` literals in tests still compile. |
| `pub fn layer_for(cause, mode)` | D14: moves `GeneratorMissed` to `RxDevice` in internal mode and changes **nothing else**. `DiagnosticCause::layer()` keeps its old body and its old meaning. |

Deviations from the plan text, all deliberate:

1. **`resolve_nominal` is a method on `CollectionConfig`, not a free function.** The plan allowed
   either. The method form means a caller cannot pass a config and a device status from
   *different* runs, which a free function permits.
2. **The diagnostic trace stays mode-blind.** The plan (§1.10) had `build_diagnostic_trace` take
   a `traffic_mode` parameter and pass it into `layer_for`. Implementing it showed that adding
   the parameter would be inert: the trace's serialized fields are cause/counters/deltas, and
   every one of them is mode-independent, so the argument would be dropped on the floor (or
   force a new payload field for information `metadata.traffic_mode` already carries). Instead
   `layer_for` is a public, tested function and the mode reaches the reader through the
   metadata — which is exactly what §1.10's own *"the display name lives in the web layer"*
   bullet says. P5's JS mirrors the same one-line rule; no Rust payload field was added.
3. **`Mismatch` gained `impl Error`.** `CollectionError::WrongDeviceMode` holds a
   `Box<Mismatch>` and needs `source()`, which requires the trait.
4. **`CollectionSummary` lost `PartialEq`/`Eq`.** `nominal_pps: Option<f64>` makes `Eq`
   impossible, and no test compared summaries by value. Structural equality was never part of
   the contract; `Debug + Clone` are.
5. **`CollectionConfig::device_endpoint()`** was added so both capture paths build the
   `DeviceEndpoint` the same way instead of each formatting host/port/origin.

- **Exit criterion met:** green suite, and the PATCH is provably unreachable from the capture
  path. Against a live `external` device the request sequence is now
  `GET /capabilities` → `GET /sensing` → `GET /csi` with no write; that sequence is asserted on
  real sockets in `device.rs`'s tests (P2) and its absence is enforced by the single-call-site
  grep above. Hardware confirmation remains P6.
- **Behaviour is unchanged in this phase by construction:** `traffic_generator` defaults to
  `External`, and no CLI can set it yet (P4). The only externally visible differences are the
  two summary lines (`device:` / `rate:` replacing `Sensing:`) and the disappearance of the
  device-side write.

### P4 — The `--internal` flag (the only behaviour-visible phase) — ✅ **IMPLEMENTED**


1. `src/bin/collect.rs` (§5): `--internal`, `--marker-port` rejection, `device`/`rate` stdout
   lines, exit code `3`.
2. `src/bin/streaming.rs` (§6): same flag, same rejection, same exit code.
3. `src/main.rs` (§8): same flag on the default binary.
4. `src/bin/visualize.rs` (§7): read the suffix back into `traffic_mode`.
5. Tests: parser tests for all four binaries, the `--marker-port` rejection message, the
   suffix nudge to `_internal`.
- **Exit criterion:** the three capture commands in Open Q7 work, and the two D3c mismatch
  directions both exit `3` with no file written.

**Status: done.** `cargo test --all-targets`: **212 passed, 0 failed** (was 198);
`cargo clippy --all-targets`: 0 warnings. Verified by *running the binaries*, not only by unit
tests, against a stub that reports a chosen `traffic_generator_mode`:

| Invocation | Stub mode | Exit | Observed |
|---|---|---|---|
| `collect --internal` | `external` | **3** | one `error:` line, **no capture file** |
| `collect --pps 20` | `ping` | **3** | the mirrored message |
| `streaming --internal` | `external` | **3** | one `error:` line, dashboard port **never bound** |
| `streaming --internal` | `ping` | 0 | reaches the dashboard (pre-flight passed) |
| `collect --internal --marker-port 5556` | — | 2 | the rejection message |
| `collect --nope` | — | 2 | usage |
| `main --pps 20` | — | 2 | this binary accepts only `--internal` |
| `main --help` | — | 0 | usage |

The stub logs any `PATCH`: **zero** were received across every run above, including the
internal-mode ones — P3's read-only invariant holds end to end.

Delivered:

| Item | Note |
|---|---|
| `--internal` on `collect`, `streaming`, `main` | sets `traffic_generator`; nothing else |
| `--output` clears `output_is_default` | in both CLIs, so an explicit name is never renamed |
| `--marker-port` + `--internal` → exit 2 | checked **after** the parse loop, so flag order does not matter (both orders are tested) |
| `EXIT_WRONG_MODE = 3` in both capture binaries | mapped from the error *type*, never from message text |
| `StreamingError::WrongDeviceMode` | new variant — see deviation 1 |
| `main` returns `ExitCode` | was `Result<(), CollectionError>`, which cannot express 2 or 3 |

Deviations from the plan text, all deliberate:

1. **`StreamingError` gained a `WrongDeviceMode` variant instead of reusing `InvalidConfig`.**
   P3 had flattened the mismatch into `InvalidConfig(format!("{mismatch} (fix with: …)"))`. Exit
   code 3 has to distinguish "the device needs switching" from "this configuration is wrong",
   and inferring that from a string prefix would be a bug waiting for a rewording. The variant
   also restores the `source()` chain to the real `Mismatch`, and the remedy command is now
   rendered by `Mismatch::display` where it belongs. `CollectionError` already had the
   equivalent variant, so the two capture paths now agree on the taxonomy.
2. **`main` no longer returns `Result`.** `--internal` and exit codes 2/3 are incompatible with
   `#[tokio::main] -> Result<_, _>` (which always exits 1). Its parser returns
   `Ok(None)` for `--help` rather than a dedicated `Help` variant, since there is only one action.
3. **`collect`/`streaming`'s `ParsedArgs` derive `Debug`** so tests can use `expect_err`.
4. **`visualize`'s provenance was extracted into `offline_nominal_source()`.** The wiring landed
   in P3 inside `main`, where the parser tests could not reach it; naming the rule made the
   "`--pps` is the only rate an offline run knows" decision testable. `--pps` absence still
   leaves the nominal `None` rather than promoting the measured rate.
5. **`main`'s tests are new** (the binary had none). They assert that `--pps 20` is an *error*
   rather than silently ignored, which is the one way this flag set could regress into
   surprising behaviour: before P4 the binary accepted nothing, so an ignored flag would
   silently leave the run sending markers.

- **Exit criterion met:** both D3c mismatch directions exit `3` with no file written, and the
  three capture commands work. The README/`docs` write-up of those commands remains P7.

### P5 — Web UI (label only) — ✅ **IMPLEMENTED**


1. `index.html`: the **Traffic** row in the Packet Rate card (§4.1).
2. `visualizer.js`: the `(traffic_mode, nominal_pps_source)` hint table (§4.2) and the
   mode-aware diagnostic labels `CAUSE_LABELS`/`CAUSE_LAYERS`/`OVERVIEW_TIPS` (§4.3, D14).
3. Manual checklist: both modes, plus a legacy payload with neither field (§4.4).
- **Exit criterion:** a live internal run and `visualize capture_v8_internal.bin` show
  **Traffic: Internal** with the device nominal; a payload without the new fields is unchanged.
- **Deferrable:** the Rust side is complete and correct without this phase; it only affects
  what a human sees.

**Status: done.** The Rust suite is unchanged and still green (**212 passed, 0 failed**;
clippy 0 warnings); this phase touches only `web/`. Added
`web/test/traffic_mode_ui.js` (**46 checks, exit 0**) — a headless harness that loads the
**shipped** `visualizer.js` in a `vm` context with a stub DOM and asserts the §4 tables, so
they are verified rather than eyeballed. Run it with `node web/test/traffic_mode_ui.js`.

**It found a real crash.** `overviewTip()` and `renderOverview()` live on `DiagnosticView`, but
`getTrafficMode()` is defined on `CsiVisualizer` — which owns the payload. Calling
`this.getTrafficMode()` from the diagnostic view threw `TypeError: this.getTrafficMode is not a
function`, so **hovering the legend row or opening the overview panel would have broken the
panel** for every payload. Fixed with a `trafficMode()` accessor on `DiagnosticView` that
delegates to `this.visualizer` (falling back to its own `data` copy), which is how every other
cross-class read in that file already works. This is exactly the class of bug that a label-only
change invites, and the reason the harness is now in the repo.

Verified end to end against **real payloads** produced by the shipped `visualize`, not
hand-written JSON:

| Payload | Traffic | Nominal | Effective | Hint | Grid slots |
|---|---|---|---|---|---|
| `samples/capture_v8.bin` (no suffix) | `—` | 20 *(muted)* | 100.01 | No `--pps` argument — default 20 PPS | 20 |
| `capture_v8_internal.bin` | **Internal** | 20 *(muted)* | 100.01 | `--pps` fallback (device rate unavailable) | 20 |
| `capture_v8_internal.bin --pps 100` | **Internal** | 100 | 100.01 | `--pps` fallback (device rate unavailable) | 100 |

The middle row is the case the Traffic row exists for: before P5, `Internal` + `20 PPS` looked
like "we drive 20", when 20 is only the fallback. The served bundle was also confirmed to carry
all three changes (`/`, `/visualizer.js`, `/style.css`), since they are `include_str!`-embedded
rather than read from disk at run time.

Delivered:

| Item | Note |
|---|---|
| `.metric-pair` **Traffic** row, `id="metaTrafficMode"` | above Nominal; `Internal`/`External`/`—`, muted when unknown |
| `.metric-pair-mode` in `style.css` | the mode is a name, not a measurement: UI font at label weight instead of tabular mono |
| `getRequestedPps()` | now returns a number only for `nominal_pps_source === "argument"` **or an absent source** (§4.2). A device-sourced nominal is no longer attributed to `--pps`. |
| `getNominalSource()` / `getTrafficMode()` | payload accessors, the single place the metadata is read |
| `getNominalPps()` | device-sourced nominals **do** drive the slot grid (§4.2), independently of the "requested" question |
| `nominalHint(mode, source, nominal, hasArgument)` | the §4.3 table as one pure function |
| `causeLabel()` / `causeLayer()` | D14: `generator_missed` → "device generator or firmware" + layer `rx` in internal mode, **unchanged otherwise** |
| `OVERVIEW_TIPS_INTERNAL` + `overviewTip()` | the tooltip follows the same rule; the bucket is never removed |

Deviations from the plan text, all deliberate:

1. **The mode-aware labels are functions, not mutated tables.** §4.3 implied overriding
   `CAUSE_LABELS`/`CAUSE_LAYERS`; mutating module-level constants per render would make the
   result depend on call order and leak between payloads. `causeLabel(cause, mode)` and
   `causeLayer(cause, mode)` read small `*_INTERNAL` overrides instead, mirroring `layer_for()`
   in Rust — same shape, same defaults, one place to change.
2. **`DiagnosticView` gained `trafficMode()`.** Required by the crash above; also removes the
   duplicated "where does the mode come from" question inside that class.
3. **The hint's `title` explains the two causes of the fallback.** `internal + argument` reads
   identically whether the device read failed (live) or an offline `.bin` never recorded
   `csi_target_pps`. The plan mandates one string for both, so the string is kept and the
   tooltip distinguishes them — the card text stays a pure function of `(mode, source)` as
   specified, and the ambiguity is answered where there is room for it.
4. **The `Traffic` row is muted when `traffic_mode` is null**, reusing the existing
   `metric-pair-default` treatment, so "unknown" is visually distinct from "External".

- **Exit criterion met** for the offline half (`visualize capture_v8_internal.bin` → **Traffic:
  Internal**). The live half needs a device and belongs to P6.

### P6 — Verification on hardware


1. The three-command README flow (§11) end to end.
2. **Request-count regression:** an external run against an `external` device issues
   `GET /capabilities` → `GET /sensing` → `GET /csi`, and **no PATCH** (assert by reading the
   device log or a proxy).
3. Byte-identical capture check: the same device, same duration, `collect` before and after
   P3 with the same flags → identical `.bin` bytes (the write removal must not affect the
   stream).
4. Live nominal check: `streaming --internal` shows the device's `csi_target_pps` **from the
   first frame**, not 20 then 100 (this is the D12b regression test, and the one that would
   have caught the original plan's error).
5. Wrong-mode stops: both directions, exit `3`, no file created.
- **Exit criterion:** all five observed, recorded in `Memory.md`.

### P7 — Documentation — ✅ **IMPLEMENTED**

1. `docs_EN` → `docs_VN` (collection, streaming, the new `device` binary) (§9).
2. `README.md`: the three-command mode-switching flow (§11).
3. `Memory.md`: the read-only decision and the pre-flight rule (§9).
- **Exit criterion:** every claim in the plan's Docs checklist (§Docs) is ticked.

**Status: done.** New `docs_EN/device.md` + `docs_VN/device.md` for the `device`
module (the two-mode table, `DeviceStatus`, the `require_mode` matrix, the
read-after-write rule, `DeviceError`, the `device` binary and its exit codes, and why
the capture path never writes), indexed in both `docs_*/README.md`. `docs_EN`/`docs_VN`
collection and visualization updated with the `--internal` mode, the `_internal`
suffix, `WrongDeviceMode`, the new `CollectionSummary` fields, the `--duration` deadline
semantics, `visualize --duration` and the `records_past_span` / `total_slots` slot
semantics; `README.md` gains `device.rs`/`bin/device.rs` in the structure list, a
`device --show` step in the quick start, and the corrected endpoint `192.168.12.144`.

### P8 — `--duration` belongs to the visualization, not to the writer — ✅ **IMPLEMENTED**

The `ce977ee` attempt cut the last frame out of the capture file so that a 5 s run at
100 PPS would contain exactly 500 records. That was the wrong layer: it made the writer
inspect frames it only has to copy, and it silently altered the recording.

1. **Reverted** the collection-side cutting (`DurationCut`, frame inspection, its tests).
   `max_duration` is a wall-clock deadline on the reader again: every chunk received is
   written whole, so a 5 s run at 100 PPS writes ~501 records (`f22cad0`/`14ab9ed`
   behaviour).
2. **`visualize --duration DUR`** reuses the `collect` duration grammar and sets
   `VisualizationOptions::span_us`, so `total_slots = round(span × slots_per_second)` is
   500 for a 5 s run — the denominator describes the run that was *asked for* while the
   file stays a faithful record of what arrived.
3. **Round once:** `slots_in_span` is integer arithmetic with a single `+ 500_000`
   rounding; the 2-dp rounding applies only to the displayed `duration_seconds`.
4. **Records past the span are counted, not trimmed:** `DiagnosticTrace::records_past_span`
   reports them, they stay in the grid, and `count_filled_slots` skips the slots outside
   `total_slots` so coverage and denominator describe the same span.
5. **Metadata** carries `declared_span_seconds`, so the payload says which span the slot
   counts belong to.
6. `--duration` is out of scope for `streaming`.

- **Exit criterion met:** `capture_v8_internal.bin` at `--pps 100` reports
  **455 filled / 501 total** with `past span: 1`, and **454 filled / 500 total** with
  `--duration 5` — the one record at second 5 slot 0 is the record the smaller
  denominator excludes.

### Not this project (recorded, not scheduled)

- **ESPectre doc** (§10): `docs/ESpectre_sdk.md` in the `@espectre` checkout, plus the
  cross-reference line in `SDK.md` §Traffic destination. Separate checkout, separate commit;
  write the borrowed-endpoint table last so it can cite the final flag names.
- Exposing `DELETE /sensing/overrides` and correcting report `261117_ESP_IDF_firmware_flash`
  §3.2.3 — ESPectre-side work, tracked there.
- **Later, in this repo** (explicitly out of scope here): `--generator ping|wifi_raw|external`
  so `collect` can *select* a mode through `device::set_traffic_generator_mode()` in the same
  invocation rather than making mode changes a separate step; an offline `--traffic-mode`
  override for `visualize`, for names the suffix cannot carry. Separate changes, separate
  decisions.

### Dependency graph

```
P1 ──┬─> P2                          (writer, independent of the capture path)
     └─> P3 ──> P4 ──> P5 ──> P8     (the `--duration` split, writer + visualizer)
                     └─> P7          (docs written; P6, the hardware run, is still open)
```

## Open questions

1. ~~**Where does the device target PPS come from?**~~ **Settled:** the capture path issues
   `GET /sensing` and reads the top-level `csi_target_pps` integer (verified: `@espectre`
   `native_direct_frontend.cpp:333`, `docs/API.md:105`). It is the **primary** source in
   internal mode; `--pps` is the fallback (D7/D11). The same response supplies
   `traffic_generator_mode`, which powers the D3c mode check. Sub-question raised here and now
   **settled: yes, the read also happens in external mode** (D12) — it replaces the removed
   PATCH, so the request count is unchanged and the mismatch can be reported before a sparse
   capture is written.
2. ~~**`--pps` with `--internal`**~~ **Settled (your call):** `--pps` is the **fallback**, used
   only when the device read fails (D11). Not rejected; not an override.
3. ~~**Is `nominal_pps_source` still needed?**~~ **Settled:** yes, and it is what makes rung 2
   honest — without it the card could not tell "device 100" from "`--pps` 50 because the read
   failed". Values: `"device"` \| `"argument"` \| `null` (D8b).
4. ~~**Should `traffic_mode` drive the diagnostic view?**~~ **Settled (your call), then
   corrected:** yes, the diagnostic view becomes mode-aware — but by **relabelling**, not by
   removing the bucket (D14). The first draft folded `generator_missed` into `device_drop`,
   which was wrong: `DeviceDrop` is defined as a stream gap with `delta_drop > 0`, whereas the
   `generator_missed` branch has contiguous counters and returns `dropped_frames = 0`,
   `delta_drop = 0`. A folded slot would have carried zero dropped frames under a "drop" label.
   The bucket therefore keeps its count and its evidence; only its display name
   ("device generator or firmware") and layer (`rx`) change, and only when the mode is internal.
   This also settles the enum question: the variant is never deleted and never unreachable-except-
   in-internal — it stays the normal result of the `delta_stream == 1`, non-low-RSSI branch, so
   old payloads deserialize and no `None` variant is needed in the enum. The `Option` lives in
   `VisualizationOptions.traffic_mode`, which is what carries "unknown".
5. ~~**Capture provenance**~~ **Settled (your call):** filename **suffix**, `_internal`, applied
   only when `--output` was not given (D15) — **and read back**, not ignored: `visualize` parses
   the suffix into `traffic_mode` (§7, §1.11) so an offline capture shows `Traffic: Internal`
   instead of `—`. Accepted cost: a renamed file can misreport its mode, which is documented as
   "a hint, never a proof". `nominal_pps` is *not* restored by the suffix, because the file
   cannot know the device's `csi_target_pps`.
6. ~~**`configured_pps` type**~~ **Settled:** it keeps `u32`. It reports the `--pps` value in
   force, which in internal mode is the fallback rate (D11) — true whether or not the client
   drives anything. Who drives traffic is carried by `traffic_mode`, not by nulling a rate that
   still means something. No public-API break, and `collect`'s existing `"… at {} PPS"` line
   keeps working as-is.
7. ~~**Should `--internal` land on the default `main` binary?**~~ **Settled (your call):** yes
   (§8). Consequence: `main.rs` gains a tiny argument parser and the `--help` text, and the
   plan's scope line now covers three binaries.
8. ~~**Failed device read**~~ **Settled, and re-scoped by the read-only change:** a failed read
   is no longer only "the `--pps` fallback fires". It now has two distinct consequences, and
   they must not be conflated:
   - **internal mode** — the nominal falls back to `--pps` (D11) and the mode cannot be
     verified, so the run **continues with a warning** on stderr
     (`device: unreachable — traffic_generator_mode not verified`). A dead device must not be
     turned into a configuration error.
   - **external mode** — the nominal needs no device value at all (D7), so a failed read costs
     nothing but the verification; same warning, run continues.
   So the read failure is *never* fatal; only a **successful read that disagrees** is (D3c).
   That split is the whole reason `require_mode` takes `Option<&DeviceStatus>` and returns
   `Ok(())` on `None`.
9. ~~**Do the report's follow-ups belong here?**~~ **Settled (your call): only the documentation
   part is in scope**, as §10 (`@espectre` `docs/ESpectre_sdk.md`), which documents internal vs
   external from the device's side and the endpoints this project borrows. **Explicitly out of
   scope for this project:** exposing `DELETE /sensing/overrides` (that is an ESPectre firmware
   feature; `clear_runtime_traffic_generator_mode()` exists but is unreachable from HTTP,
   report §4) and correcting report `261117_ESP_IDF_firmware_flash` §3.2.3. Both are ESPectre
   work, tracked there, not here — this repo neither implements nor waits on them. Remaining
   detail for §10: should the new doc live at `docs/ESpectre_sdk.md` as proposed, or be folded
   into the existing `docs/SDK.md` §Traffic destination (`:292-298`) and `docs/CSI.md` §Traffic
   sources (`:9-24`), which already cover part of it?
10. ~~**Should the capture path keep the `PATCH` in external mode?**~~ **Settled (your call): no
    — neither mode writes** (D3). Reading `GET /sensing` answers the question the write was
    standing in for, the write only ever changed NVS-persisted device state, and it left the AP
    generating no traffic after the run (D3b). The write survives as
    `device::set_traffic_generator_mode()` (§1b.4) and the `device` binary (§6b), which is where
    an explicit, operator-initiated change belongs.
11. ~~**What happens when the device is in the wrong mode?**~~ **Settled (your call): warn and
    stop** (D3c) — `error:` on stderr, exit `3`, no capture file. The alternative (warn and
    capture anyway) was rejected because it produces an empty `_internal`-named file whose name
    asserts a mode the device was not in.
12. ~~**Where does the read live?**~~ **Settled (your call): its own `device` module** (§1b,
    D13), rather than `read_device_sensing` inside `collection.rs`. Keeps the device contract in
    one file, gives `docs/ESpectre_sdk.md` a single counterpart to describe, and makes the only
    writer a module that is named for it.
13. ~~**How does `streaming` see the device's rate before it publishes?**~~ **Settled:** it
    resolves it **pre-flight in `run()`**, before anything is spawned (§3.1, D12b). The earlier
    draft ("the accumulator learns it in `run()`'s completion branches") was impossible: the
    accumulator is moved into `run_decoder_loop` at spawn (`:266`, `:516`), `CollectionSummary`
    is built only after the capture ends (`collection.rs:342-350`), and every completion branch
    aborts `server_task` before reading that result. No channel is needed once the value is
    resolved up front — the dashboard never shows a substitute.
