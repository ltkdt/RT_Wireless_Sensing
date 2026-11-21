//! End-to-end integration tests for the visualization module.
//!
//! Validates decoding and preparation of `capture_v8.bin` into the data structures
//! required for HTML Canvas 2D heatmap rendering.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
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
    visualization::{
        VisualizationData, VisualizationError, VisualizationOptions,
        prepare_visualization_data_with_options,
    },
};

#[test]
fn capture_decodes_into_visualization_matrix() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");

    let data = VisualizationData::from_file(&input).expect("should decode capture_v8.bin");

    // 1. Verify Metadata
    assert_eq!(data.metadata.total_frames, 691);
    assert_eq!(data.metadata.subcarriers, 64);
    assert_eq!(data.metadata.title, "CSI Amplitude Heatmap");
    assert!((data.metadata.duration_seconds - 34.499_075).abs() < 1e-4);
    assert!((data.metadata.effective_pps - 20.00).abs() < 0.05);

    // 2. Verify Timestamps
    assert_eq!(data.timestamps.len(), 691);
    assert_eq!(data.timestamps[0], 0.0);
    assert!((data.timestamps[690] - 34.499_075).abs() < 1e-4);

    // Monotonicity check
    for window in data.timestamps.windows(2) {
        assert!(
            window[1] > window[0],
            "timestamps must be strictly increasing"
        );
    }

    // 3. Verify Amplitudes Matrix [N, 64]
    assert_eq!(data.amplitudes.len(), 691);
    for row in &data.amplitudes {
        assert_eq!(row.len(), 64);
    }

    // Subcarrier 0 is null guard bin (0.0)
    for row in &data.amplitudes {
        assert_eq!(row[0], 0.0);
    }

    // Subcarrier 4 is first live subcarrier: (I, Q) = (-20, 5), hypot ~ 20.6155
    assert!((data.amplitudes[0][4] - 20.615_528).abs() < 1e-4);
}

#[test]
fn json_serialization_and_export() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");

    let data = VisualizationData::from_file(&input).expect("should decode capture_v8.bin");
    let json_str = data.to_json().expect("should serialize to JSON");

    assert!(json_str.contains("\"title\":\"CSI Amplitude Heatmap\""));
    assert!(json_str.contains("\"total_frames\":691"));
    assert!(json_str.contains("\"subcarriers\":64"));

    // Verify round-trip parsing
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("JSON must be valid");
    assert_eq!(parsed["metadata"]["total_frames"], 691);
    assert_eq!(parsed["amplitudes"].as_array().unwrap().len(), 691);
}

#[test]
fn standalone_html_bundle_generation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");

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
        data.metadata.nominal_pps.round().max(1.0) as usize
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

#[test]
fn diagnostic_grid_agrees_with_occupancy_on_capture() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let data = VisualizationData::from_file(&root.join("capture_v8.bin")).expect("decode");
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");

    let measured = VisualizationData::from_file(&input).expect("decode");
    let configured = VisualizationData::from_file_with_pps(&input, Some(100.0)).expect("decode");

    assert_eq!(configured.metadata.nominal_pps, 100.0);
    let trace = configured.diagnostics.as_ref().expect("trace");
    assert_eq!(trace.slots_per_second, 100);

    // The capture runs at ~20 pps, so a full second fills about 20 of 100 slots
    // instead of collapsing the grid to the measured rate.
    let full = trace
        .seconds
        .iter()
        .find(|stats| !stats.partial && stats.samples > 0)
        .expect("a full second");
    assert_eq!(full.slots, 100);
    assert!(full.filled_slots <= 21, "filled {}", full.filled_slots);
    assert!(full.empty_slots >= 79, "empty {}", full.empty_slots);

    let measured_trace = measured.diagnostics.as_ref().expect("trace");
    assert!(measured_trace.slots_per_second < 100);
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
