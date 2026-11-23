//! End-to-end integration tests for the visualization module.
//!
//! Validates decoding and preparation of `capture_v8.bin` into the data structures
//! required for HTML Canvas 2D heatmap rendering.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use rt_wireless_sensing::{
    csi_header::{
        CSI_HEADER_LEN, CsiHeader, RAW_CSI_FLAG_FRESH, RAW_CSI_FLAG_WIFI_RX_TS_VALID,
        RAW_CSI_RECORD_MAGIC, RAW_CSI_RECORD_VERSION_V8,
    },
    decoder::{
        CsiPacket, HTTP_FRAME_PREFIX_LEN_U16, HttpFrameHeader, RAW_CSI_PROTOCOL_VERSION,
        RAW_CSI_RESPONSE_MAGIC, SESSION_ID_LEN,
    },
    dsp::NormScheme,
    visualization::{
        DspOptions, VisualizationData, VisualizationError, VisualizationOptions,
        prepare_visualization_data_with_options,
    },
};

#[test]
fn capture_decodes_into_visualization_matrix() {
    let input = rt_wireless_sensing::fixtures::capture_v8();

    let data = VisualizationData::from_file(&input).expect("should decode capture_v8.bin");

    // 1. Verify Metadata
    assert_eq!(data.metadata.total_frames, 501);
    assert_eq!(data.metadata.subcarriers, 64);
    assert_eq!(data.metadata.title, "CSI Amplitude Heatmap");
    assert!((data.metadata.duration_seconds - 4.999_419).abs() < 1e-4);
    assert!((data.metadata.effective_pps - 100.01).abs() < 0.05);

    // 2. Verify Timestamps
    assert_eq!(data.timestamps.len(), 501);
    assert_eq!(data.timestamps[0], 0.0);
    assert!((data.timestamps[500] - 4.999_419).abs() < 1e-4);

    // Monotonicity check
    for window in data.timestamps.windows(2) {
        assert!(
            window[1] > window[0],
            "timestamps must be strictly increasing"
        );
    }

    // 3. Verify Amplitudes Matrix [N, 64]
    assert_eq!(data.amplitudes.len(), 501);
    for row in &data.amplitudes {
        assert_eq!(row.len(), 64);
    }

    // Subcarrier 0 is null guard bin (0.0)
    for row in &data.amplitudes {
        assert_eq!(row[0], 0.0);
    }

    // Subcarrier 4 is the first live subcarrier: raw (Q, I) = (-12, -20),
    // so I = -20, Q = -12, hypot ~ 23.3238
    assert!((data.amplitudes[0][4] - 23.323_808).abs() < 1e-4);
}

#[test]
fn json_serialization_and_export() {
    let input = rt_wireless_sensing::fixtures::capture_v8();

    let data = VisualizationData::from_file(&input).expect("should decode capture_v8.bin");
    let json_str = data.to_json().expect("should serialize to JSON");

    assert!(json_str.contains("\"title\":\"CSI Amplitude Heatmap\""));
    assert!(json_str.contains("\"total_frames\":501"));
    assert!(json_str.contains("\"subcarriers\":64"));

    // Verify round-trip parsing
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("JSON must be valid");
    assert_eq!(parsed["metadata"]["total_frames"], 501);
    assert_eq!(parsed["amplitudes"].as_array().unwrap().len(), 501);
}

#[test]
fn standalone_html_bundle_generation() {
    let input = rt_wireless_sensing::fixtures::capture_v8();
    let temp_html = std::env::temp_dir().join(format!("test_heatmap_{}.html", std::process::id()));

    let data = VisualizationData::from_file(&input)
        .expect("should decode capture_v8.bin")
        .with_classification(
            Some("WaveHand".to_string()),
            Some(0.96),
            Some("Tested on gesture capture".to_string()),
        );

    data.export_html(&temp_html).expect("should export html");

    let content = fs::read_to_string(&temp_html).expect("file should exist");
    assert!(content.contains("<!DOCTYPE html>"));
    assert!(content.contains("CSI Amplitude Heatmap"));
    assert!(content.contains("<canvas id=\"heatmapCanvas\""));
    assert!(content.contains("window.CSI_DATA ="));
    assert!(content.contains("\"WaveHand\""));
    assert!(content.contains("COLORMAPS"));

    fs::remove_file(&temp_html).ok();
}

#[test]
fn rejects_empty_input() {
    let result = VisualizationData::from_bytes(&[], Some("empty.bin"));
    assert!(matches!(result, Err(VisualizationError::NoRecords)));
}

#[test]
fn diagnostics_are_attached_and_old_payloads_still_parse() {
    let input = rt_wireless_sensing::fixtures::capture_v8();

    let data = VisualizationData::from_file(&input).expect("should decode capture_v8.bin");

    // The trace carries one row per received record, the empty slots between
    // them, and one statistics row per second; the slot column count is the
    // nominal PPS rounded once in Rust.
    let diagnostics = data
        .diagnostics
        .as_ref()
        .expect("the diagnostic trace is attached");
    assert_eq!(
        diagnostics.slots_per_second,
        data.metadata
            .nominal_pps
            .unwrap_or(data.metadata.effective_pps)
            .round()
            .max(1.0) as usize
    );
    assert_eq!(diagnostics.origin_ticks, data.metadata.start_time_us);
    assert!(diagnostics.capture_end);
    assert_eq!(diagnostics.records.len(), data.timestamps.len());
    assert!(!diagnostics.seconds.is_empty());
    assert_eq!(
        diagnostics.records[0].frame, 0,
        "records are indexed like timestamps"
    );

    // The field serializes, so the web UI can gate the toggle on it.
    let json_str = data.to_json().expect("should serialize to JSON");
    assert!(json_str.contains("\"diagnostics\":{"));
    assert!(json_str.contains("\"records\":["));
    assert!(json_str.contains("\"empty_slots\":["));
    assert!(json_str.contains("\"seconds\":["));

    let round_trip: VisualizationData =
        serde_json::from_str(&json_str).expect("payload must round-trip");
    assert_eq!(round_trip.diagnostics, data.diagnostics);

    // Payloads from before the diagnostic view (no `diagnostics` field) must
    // still deserialize, with the field as None so the toggle stays hidden.
    let mut value: serde_json::Value =
        serde_json::from_str(&json_str).expect("JSON must be valid");
    value
        .as_object_mut()
        .expect("top level object")
        .remove("diagnostics");
    let old_payload: VisualizationData =
        serde_json::from_value(value).expect("old payload must deserialize");
    assert!(old_payload.diagnostics.is_none());
}

/// One synthetic record: stream counters, device clock, and radio quality.
#[derive(Clone, Copy)]
struct Sample {
    stream_sequence: u64,
    drop: u64,
    backpressure: u64,
    ticks_us: u64,
    rssi_dbm: i8,
}

impl Sample {
    fn new(stream_sequence: u64, ticks_us: u64) -> Self {
        Self {
            stream_sequence,
            drop: 0,
            backpressure: 0,
            ticks_us,
            rssi_dbm: -35,
        }
    }

    fn rssi(mut self, rssi_dbm: i8) -> Self {
        self.rssi_dbm = rssi_dbm;
        self
    }

    fn drop(mut self, drop: u64) -> Self {
        self.drop = drop;
        self
    }

    fn backpressure(mut self, backpressure: u64) -> Self {
        self.backpressure = backpressure;
        self
    }
}

/// Build one [`CsiPacket`] from a [`Sample`] without going through the decoder.
fn packet(sample: Sample) -> CsiPacket {
    CsiPacket {
        frame: HttpFrameHeader {
            magic: RAW_CSI_RESPONSE_MAGIC,
            version: RAW_CSI_PROTOCOL_VERSION,
            record_version: RAW_CSI_RECORD_VERSION_V8,
            header_len: HTTP_FRAME_PREFIX_LEN_U16,
            session_id: [0x11; SESSION_ID_LEN],
            stream_sequence: sample.stream_sequence,
            record_len: u16::from(CSI_HEADER_LEN) + 4,
            flags: 0,
            fresh_record_total: sample.stream_sequence,
            raw_drop_total: sample.drop,
            raw_send_backpressure_total: sample.backpressure,
        },
        csi: CsiHeader {
            magic: RAW_CSI_RECORD_MAGIC,
            version: RAW_CSI_RECORD_VERSION_V8,
            header_len: CSI_HEADER_LEN,
            chip_code: 5,
            flags: RAW_CSI_FLAG_FRESH | RAW_CSI_FLAG_WIFI_RX_TS_VALID,
            seq_num: sample.stream_sequence as u32,
            num_sc: 2,
            csi_len_bytes: 4,
            device_id: 0xb78d_3a15_354a_d161,
            device_ticks_us: sample.ticks_us,
            wifi_rx_ts_us: (sample.ticks_us / 1_000) as u32,
            wifi_rx_start_ts_ns: 0,
            channel: 149,
            rssi_dbm: sample.rssi_dbm,
            noise_floor_dbm: -94,
            transport_counter: sample.backpressure,
            fresh_counter: sample.stream_sequence as u32,
            request_counter: sample.stream_sequence as u32,
            phy_mode_code: 3,
            ltf_type_code: 3,
            channel_width_code: 1,
        },
        iq_raw: vec![10, 20, -30, 40],
    }
}

/// Prepare a synthetic stream with an explicit nominal PPS and trace setting.
fn prepare_with(
    samples: &[Sample],
    origin_ticks: u64,
    pps: f64,
    diagnostics: bool,
) -> VisualizationData {
    let packets: Vec<CsiPacket> = samples.iter().copied().map(packet).collect();
    prepare_visualization_data_with_options(
        &packets,
        Some("synthetic"),
        origin_ticks,
        VisualizationOptions {
            nominal_pps: Some(pps),
            capture_end: true,
            diagnostics,
            // The synthetic fixtures are tiny; the DSP view is exercised by its
            // own tests below.
            dsp: None,
        },
    )
    .expect("synthetic stream should prepare")
}

/// Prepare a synthetic stream with the trace enabled.
fn prepare(samples: &[Sample], origin_ticks: u64, pps: f64) -> VisualizationData {
    prepare_with(samples, origin_ticks, pps, true)
}

/// The diagnostic trace is opt-out: `diagnostics: false` must drop it from the
/// payload, while the default options keep it. This is the contract the live
/// dashboard relies on to keep its 500 ms polls small.
#[test]
fn diagnostic_trace_is_optional() {
    let origin = 5_000_000;
    let samples = [
        Sample::new(1, origin),
        Sample::new(2, origin + 10_000),
        Sample::new(4, origin + 1_030_000),
    ];

    let packets: Vec<CsiPacket> = samples.iter().copied().map(packet).collect();
    let without = prepare_visualization_data_with_options(
        &packets,
        Some("synthetic"),
        origin,
        VisualizationOptions {
            nominal_pps: Some(100.0),
            capture_end: true,
            diagnostics: false,
            dsp: None,
        },
    )
    .expect("synthetic stream should prepare");
    assert!(without.diagnostics.is_none());
    assert!(
        serde_json::to_string(&without)
            .expect("serialize")
            .contains("\"diagnostics\":null"),
        "an absent trace must serialize as null so the web layer hides the toggle"
    );

    // The default options keep the trace for offline payloads.
    let with_defaults = prepare_visualization_data_with_options(
        &packets,
        Some("synthetic"),
        origin,
        VisualizationOptions {
            nominal_pps: Some(100.0),
            ..VisualizationOptions::default()
        },
    )
    .expect("synthetic stream should prepare");
    assert!(with_defaults.diagnostics.is_some());

    // The heatmap matrix is identical either way: only the trace is dropped.
    assert_eq!(without.timestamps, with_defaults.timestamps);
    assert_eq!(without.amplitudes, with_defaults.amplitudes);
    assert_eq!(without.metadata, with_defaults.metadata);
}

/// `--no-dsp`: the block is absent but the base payload is untouched.
#[test]
fn dsp_view_is_optional() {
    let origin = 5_000_000;
    let samples: Vec<Sample> = (0..40)
        .map(|i| Sample::new(1, origin + i * 50_000))
        .collect();
    let packets: Vec<CsiPacket> = samples.iter().copied().map(packet).collect();

    let build = |dsp| {
        prepare_visualization_data_with_options(
            &packets,
            Some("synthetic"),
            origin,
            VisualizationOptions {
                nominal_pps: Some(20.0),
                capture_end: true,
                diagnostics: false,
                dsp,
            },
        )
        .expect("synthetic stream should prepare")
    };

    let without = build(None);
    assert!(without.dsp.is_none());
    assert!(
        serde_json::to_string(&without)
            .expect("serialize")
            .contains("\"dsp\":null"),
        "an absent DSP view must serialize as null so the web layer hides the toggle"
    );

    let with = build(Some(DspOptions::default()));
    let dsp = with.dsp.as_ref().expect("dsp view");
    assert_eq!(with.timestamps, without.timestamps);
    assert_eq!(with.amplitudes, without.amplitudes);
    assert_eq!(with.metadata, without.metadata);
    assert_eq!(dsp.target_fs, 20.0);
    assert_eq!(dsp.interp, "cubic_spline");
    assert_eq!(dsp.norm, None);
    assert_eq!(dsp.max_gap_ms, None);
    assert_eq!(dsp.panels.len(), 2);
    assert_eq!(dsp.panels[0].id, "amplitude");
    assert_eq!(dsp.panels[1].id, "phase");
}

/// The DSP grid is the diagnostic slot grid: column `j` is `(j / N, j % N)`.
#[test]
fn dsp_grid_matches_slot_grid_on_capture() {
    let data =
        VisualizationData::from_file_with_pps(&rt_wireless_sensing::fixtures::capture_v8(), Some(100.0))
            .expect("decode");
    let trace = data.diagnostics.as_ref().expect("trace");
    let dsp = data.dsp.as_ref().expect("dsp view");

    let slots = trace.slots_per_second;
    assert_eq!(slots, 100);
    assert!((dsp.target_fs - slots as f64).abs() < 1e-9);

    // Grid columns are uniform at 1/target_fs and span the capture: the grid
    // must cover the whole duration at the requested rate, not just the
    // measured one.
    let expected_columns = (data.metadata.duration_seconds * slots as f64) as usize;
    assert!(
        dsp.timestamps.len() >= expected_columns,
        "grid has {} columns, expected at least {expected_columns} for {:.3}s at {slots} Hz",
        dsp.timestamps.len(),
        data.metadata.duration_seconds
    );
    for w in dsp.timestamps.windows(2) {
        assert!((w[1] - w[0] - 1.0 / dsp.target_fs).abs() < 1e-9);
    }
    assert!((dsp.timestamps[0] - data.timestamps[0]).abs() < 1e-9);

    // Column j maps to (second, slot) = (j / N, j % N), which must be a
    // non-decreasing pair across the grid.
    let mut previous = (0usize, 0usize);
    for (j, _) in dsp.timestamps.iter().enumerate().step_by(97) {
        let key = (j / slots, j % slots);
        assert!(
            key.0 > previous.0 || (key.0 == previous.0 && key.1 >= previous.1),
            "column {j} broke slot order: {key:?} after {previous:?}"
        );
        previous = key;
    }

    // Every panel has the grid shape and no value is NaN (no masked columns
    // in this capture with the default options).
    assert_eq!(dsp.time_state.len(), dsp.timestamps.len());
    for panel in &dsp.panels {
        assert_eq!(panel.data.len(), dsp.timestamps.len());
        for row in &panel.data {
            assert_eq!(row.len(), data.metadata.subcarriers);
            assert!(row.iter().all(|v| v.is_finite()));
        }
    }

    // Phase is a real angle, not a placeholder: inside ±π with variation.
    let phase = &dsp.panels[1].data;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for row in phase {
        for value in row {
            assert!(*value >= -std::f32::consts::PI - 1e-5 && *value <= std::f32::consts::PI + 1e-5);
            min = min.min(*value);
            max = max.max(*value);
        }
    }
    assert!(max - min > 0.1, "phase is constant: {min}..{max}");
}

/// The null-bin table on a real 64-subcarrier capture is exactly HT20's.
#[test]
fn dsp_null_bins_on_capture_are_the_ht20_table() {
    let data =
        VisualizationData::from_file_with_pps(&rt_wireless_sensing::fixtures::capture_v8(), Some(100.0))
            .expect("decode");
    let dsp = data.dsp.as_ref().expect("dsp view");
    assert_eq!(dsp.null_subcarriers, vec![0, 1, 2, 3, 32, 61, 62, 63]);
}

/// A normalization adds the third panel and leaves the resampled pair intact.
#[test]
fn dsp_normalization_adds_a_panel() {
    let plain =
        VisualizationData::from_file_with_pps(&rt_wireless_sensing::fixtures::capture_v8(), Some(100.0))
            .expect("decode");
    let base = plain.dsp.as_ref().expect("dsp view");

    let bytes = fs::read(rt_wireless_sensing::fixtures::capture_v8()).expect("read capture");
    let normalized = VisualizationData::from_bytes_with_options(
        &bytes,
        Some("capture_v8.bin"),
        VisualizationOptions {
            nominal_pps: Some(100.0),
            diagnostics: false,
            dsp: Some(DspOptions {
                norm: Some(NormScheme::PerSample),
                ..DspOptions::default()
            }),
            ..VisualizationOptions::default()
        },
    )
    .expect("prepare");
    let dsp = normalized.dsp.as_ref().expect("dsp view");
    assert_eq!(dsp.norm.as_deref(), Some("per_sample"));
    assert_eq!(dsp.panels.len(), 3);
    assert_eq!(dsp.panels[0].id, "amplitude");
    assert_eq!(dsp.panels[1].id, "phase");
    let norm = &dsp.panels[2];
    assert_eq!(norm.id, "amplitude_norm");
    assert_eq!(norm.kind, "diverging");

    // The base panels are unchanged by the normalization.
    assert_eq!(base.panels[0].data, dsp.panels[0].data);
    assert_eq!(base.panels[1].data, dsp.panels[1].data);

    // Null subcarriers are zero in the normalized matrix.
    let null = &dsp.null_subcarriers;
    for row in &norm.data {
        for k in null {
            assert_eq!(row[*k], 0.0);
        }
    }
}

/// A gap threshold masks columns instead of fabricating values there, while a
/// gap under the threshold is reported as bridged.
#[test]
fn dsp_max_gap_masks_columns() {
    let origin = 5_000_000;
    // 80 samples at 50 ms with two holes: 0.3 s after index 20 (over the
    // 200 ms threshold -> masked) and 0.15 s after index 60 (over the
    // ~1.5 x 50 ms bridging limit, under the threshold -> bridged). The grid
    // runs at 100 Hz, so both holes span several columns. Values are explicit
    // so the expectation stays readable.
    let samples: Vec<Sample> = (0..80u64)
        .map(|i| {
            let mut offset = 0u64;
            if i > 20 {
                offset += 300_000;
            }
            if i > 60 {
                offset += 100_000;
            }
            Sample::new(1, origin + i * 50_000 + offset)
        })
        .collect();
    let packets: Vec<CsiPacket> = samples.iter().copied().map(packet).collect();

    let data = prepare_visualization_data_with_options(
        &packets,
        Some("synthetic"),
        origin,
        VisualizationOptions {
            nominal_pps: Some(100.0),
            capture_end: true,
            diagnostics: false,
            dsp: Some(DspOptions {
                max_gap_ms: Some(200.0),
                ..DspOptions::default()
            }),
        },
    )
    .expect("prepare");
    let dsp = data.dsp.as_ref().expect("dsp view");
    assert_eq!(dsp.max_gap_ms, Some(200.0));

    let masked: Vec<usize> = dsp
        .time_state
        .iter()
        .enumerate()
        .filter(|(_, state)| **state == 2)
        .map(|(j, _)| j)
        .collect();
    let bridged: Vec<usize> = dsp
        .time_state
        .iter()
        .enumerate()
        .filter(|(_, state)| **state == 1)
        .map(|(j, _)| j)
        .collect();
    assert!(!masked.is_empty(), "the 0.3 s hole must mask columns");
    assert!(!bridged.is_empty(), "the 0.15 s hole must be bridged: {:?}", dsp.time_state);
    // The masked hole comes first in time, the bridged one after it.
    assert!(*masked.last().unwrap() < *bridged.first().unwrap());

    // Masked cells are withheld (NaN) rather than presented as values; the
    // bridged column itself carries an interpolated value.
    for j in &masked {
        assert!(dsp.panels[0].data[*j].iter().all(|v| v.is_nan()));
    }
    assert!(dsp.panels[0].data[bridged[0]].iter().all(|v| v.is_finite()));
    // Untouched head columns stay finite.
    assert!(dsp.panels[0].data[0].iter().all(|v| v.is_finite()));
}

#[test]
fn diagnostic_grid_agrees_with_occupancy_on_capture() {
    let data = VisualizationData::from_file(&rt_wireless_sensing::fixtures::capture_v8()).expect("decode");
    let trace = data.diagnostics.as_ref().expect("trace");
    let slots = trace.slots_per_second;

    // Reimplement the web occupancy formula: slot = floor((t - second) * slots + 1e-6).
    let mut expected: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
    for timestamp in &data.timestamps {
        let second = timestamp.floor() as u32;
        let slot = (((timestamp - f64::from(second)) * slots as f64) + 1e-6).floor() as usize;
        expected
            .entry(second)
            .or_default()
            .insert(slot.min(slots - 1));
    }

    assert_eq!(trace.seconds.len(), expected.len());
    for stats in &trace.seconds {
        let filled = expected.get(&stats.second).map_or(0, BTreeSet::len);
        assert_eq!(
            stats.filled_slots, filled,
            "second {} filled slots must match the occupancy formula",
            stats.second
        );
        assert_eq!(stats.empty_slots, slots - filled);
    }

    // Every record lands in a slot the occupancy formula also fills.
    for record in &trace.records {
        assert!(
            expected
                .get(&record.second)
                .is_some_and(|set| set.contains(&(record.slot as usize))),
            "record {} at second {} slot {} is not in the occupancy grid",
            record.frame,
            record.second,
            record.slot
        );
    }
}

#[test]
fn explicit_pps_keeps_the_slot_grid_at_the_configured_rate() {
    let input = rt_wireless_sensing::fixtures::capture_v8();

    let measured = VisualizationData::from_file(&input).expect("decode");
    // The fixture runs at ~100 pps. Ask for 200 so the configured grid is
    // provably *not* the measured rate: the same records are then spread over
    // twice as many slots, leaving the odd ones empty.
    let configured = VisualizationData::from_file_with_pps(&input, Some(200.0)).expect("decode");

    assert_eq!(configured.metadata.nominal_pps, Some(200.0));
    let trace = configured.diagnostics.as_ref().expect("trace");
    assert_eq!(trace.slots_per_second, 200);

    // A full second holds 200 slots but only ~100 records, so about half the
    // slots are filled. The point of the test is that the grid follows the
    // configured rate instead of collapsing to the measured one.
    let full = trace
        .seconds
        .iter()
        .find(|stats| !stats.partial && stats.samples > 0)
        .expect("a full second");
    assert_eq!(full.slots, 200);
    assert!(
        full.filled_slots <= 101,
        "filled {} of 200",
        full.filled_slots
    );
    assert!(
        full.empty_slots >= 99,
        "empty {} of 200",
        full.empty_slots
    );

    let measured_trace = measured.diagnostics.as_ref().expect("trace");
    assert!(measured_trace.slots_per_second < 200);
}

#[test]
fn capture_summary_aggregates_empty_slot_causes() {
    // 10 slots per second: slot i starts at i * 100 ms. One gap of each
    // attributable kind, plus a trailing slot with no closing record:
    //
    //   slot 0 filled | 1 device_drop | 2 filled | 3 desktop_backpressure
    //   slot 4 filled (weak) | 5 air_loss | 6 filled | 7 generator_missed
    //   slot 8 filled | 9 ambiguous (no closing record)
    let origin = 1_000_000;
    let samples = [
        Sample::new(1, origin),
        // Slot 1: the stream skipped two frames and the device counted both as
        // raw drops -> device_drop, 2 dropped frames.
        Sample::new(4, origin + 200_000).drop(2),
        // Slot 3: the stream skipped one frame and the desktop reported
        // backpressure -> desktop_backpressure, 1 dropped frame.
        Sample::new(6, origin + 400_000).backpressure(1),
        // Slot 4 is weak and slot 6 is normal, so slot 5 is the only gap
        // bordered by a weak record -> air_loss.
        Sample::new(7, origin + 450_000).rssi(-80),
        Sample::new(8, origin + 600_000),
        // Slot 7: contiguous counters and normal RSSI on both sides ->
        // generator_missed.
        Sample::new(9, origin + 800_000),
        // Slot 8 is filled by the last record; slot 9 has no closing record
        // after it -> ambiguous.
        Sample::new(10, origin + 850_000),
    ];    let data = prepare(&samples, origin, 10.0);
    let trace = data.diagnostics.as_ref().expect("trace");
    let summary = &trace.summary;

    // Covered span: slots 0..=9 of second 0, i.e. 7 filled + 5 empty.
    assert_eq!(summary.filled_records, 7);
    assert_eq!(summary.empty_slots, 5);
    assert_eq!(summary.total_slots, 12);
    assert!((summary.loss_ratio - 5.0 / 12.0).abs() < 1e-12);

    assert_eq!(summary.cause_counts.device_drop, 1);
    assert_eq!(summary.cause_counts.desktop_backpressure, 1);
    assert_eq!(summary.cause_counts.air_loss, 1);
    assert_eq!(summary.cause_counts.generator_missed, 1);
    assert_eq!(summary.cause_counts.ambiguous, 1);

    // The five cause buckets partition the empty slots.
    let counted = summary.cause_counts.device_drop
        + summary.cause_counts.desktop_backpressure
        + summary.cause_counts.air_loss
        + summary.cause_counts.generator_missed
        + summary.cause_counts.ambiguous;
    assert_eq!(counted, summary.empty_slots);

    // Σ(Δstream − 1) over the empty slots: 2 from the device gap, 1 from the
    // backpressure gap, 0 from the contiguous ones.
    assert_eq!(summary.dropped_frames_total, 3);
    assert_eq!(summary.delta_drop_total, 2);
    assert_eq!(summary.delta_backpressure_total, 1);
    assert_eq!(summary.empty_slots_low_rssi, 1);

    // A payload written before `summary` existed still deserializes, with the
    // field defaulted, so old captures keep loading.
    let mut value = serde_json::to_value(&data).expect("serialize");
    let diagnostics = value
        .get_mut("diagnostics")
        .and_then(|diagnostics| diagnostics.as_object_mut())
        .expect("diagnostics object");
    diagnostics.remove("summary");
    let restored: VisualizationData = serde_json::from_value(value).expect("deserialize");
    let restored_trace = restored.diagnostics.as_ref().expect("trace");
    assert_eq!(restored_trace.summary, Default::default());
    // The rest of the trace is untouched by the missing field.
    assert_eq!(restored_trace.records.len(), trace.records.len());
    assert_eq!(restored_trace.empty_slots.len(), trace.empty_slots.len());
}
