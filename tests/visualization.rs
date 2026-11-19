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
        DiagnosticCause, DiagnosticLayer, VisualizationData, VisualizationError,
        VisualizationOptions, prepare_visualization_data_with_options,
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

/// Prepare a synthetic stream with an explicit nominal PPS.
fn prepare(samples: &[Sample], origin_ticks: u64, pps: f64) -> VisualizationData {
    let packets: Vec<CsiPacket> = samples.iter().copied().map(packet).collect();
    prepare_visualization_data_with_options(
        &packets,
        Some("synthetic"),
        origin_ticks,
        VisualizationOptions {
            nominal_pps: Some(pps),
            capture_end: true,
        },
    )
    .expect("synthetic stream should prepare")
}

#[test]
fn diagnostic_records_carry_deltas_and_slot_positions() {
    let origin = 5_000_000;
    let samples = [
        Sample::new(1, origin),
        Sample::new(2, origin + 10_000),
        Sample::new(4, origin + 25_000),
        Sample::new(5, origin + 1_030_000),
    ];
    let data = prepare(&samples, origin, 100.0);
    let trace = data.diagnostics.as_ref().expect("trace");

    assert_eq!(trace.slots_per_second, 100);
    assert_eq!(trace.origin_ticks, origin);
    assert_eq!(trace.records.len(), 4);

    let first = &trace.records[0];
    assert_eq!((first.second, first.slot), (0, 0));
    assert_eq!(first.slot_offset_us, 0);
    assert_eq!(first.delta_stream, None);
    assert_eq!(first.delta_ticks_us, None);
    assert_eq!(first.delta_wifi_rx_us, None);
    assert_eq!(first.snr_db, Some(-35 - -94));

    let second = &trace.records[1];
    assert_eq!((second.second, second.slot), (0, 1));
    assert_eq!(second.slot_offset_us, 0);
    assert_eq!(second.delta_stream, Some(1));
    assert_eq!(second.delta_ticks_us, Some(10_000));
    assert_eq!(second.delta_wifi_rx_us, Some(10));

    // 25 ms into the second is slot 2 of 100, 5 ms after the slot start.
    let third = &trace.records[2];
    assert_eq!((third.second, third.slot), (0, 2));
    assert_eq!(third.slot_offset_us, 5_000);
    assert_eq!(third.delta_stream, Some(2));
    assert_eq!(third.delta_ticks_us, Some(15_000));

    let fourth = &trace.records[3];
    assert_eq!((fourth.second, fourth.slot), (1, 3));
    assert_eq!(fourth.slot_offset_us, 0);
    assert_eq!(fourth.delta_ticks_us, Some(1_005_000));
}

#[test]
fn empty_slots_are_classified_by_layer() {
    let origin = 1_000_000;
    let samples = [
        Sample::new(1, origin),
        Sample::new(2, origin + 10_000),
        // Slots 2 and 3: the stream skipped two frames and the device counted
        // both as raw drops.
        Sample::new(5, origin + 40_000).drop(2),
        Sample::new(6, origin + 50_000),
        // Slot 6: the stream skipped one frame and the desktop reported
        // backpressure.
        Sample::new(8, origin + 70_000).backpressure(1),
        // Slot 9: contiguous counters, but the record before it is weak.
        Sample::new(9, origin + 80_000).rssi(-80),
        Sample::new(10, origin + 100_000),
        // Slot 11: contiguous counters and normal RSSI.
        Sample::new(11, origin + 120_000),
    ];
    let data = prepare(&samples, origin, 100.0);
    let trace = data.diagnostics.as_ref().expect("trace");

    let empty = |slot: u32| {
        trace
            .empty_slots
            .iter()
            .find(|empty| empty.second == 0 && empty.slot == slot)
            .unwrap_or_else(|| panic!("slot {slot} should be empty"))
    };

    let device_drop = empty(2);
    assert_eq!(device_drop.cause, DiagnosticCause::DeviceDrop);
    assert_eq!(device_drop.cause.layer(), DiagnosticLayer::RxDevice);
    assert_eq!(device_drop.dropped_frames, 2);
    assert_eq!(device_drop.delta_drop, 2);
    assert_eq!(device_drop.previous.as_ref().map(|b| b.seq_num), Some(2));
    assert_eq!(device_drop.next.as_ref().map(|b| b.seq_num), Some(5));
    assert_eq!(empty(3).cause, DiagnosticCause::DeviceDrop);

    let desktop = empty(6);
    assert_eq!(desktop.cause, DiagnosticCause::DesktopBackpressure);
    assert_eq!(desktop.cause.layer(), DiagnosticLayer::Desktop);
    assert_eq!(desktop.dropped_frames, 1);
    assert_eq!(desktop.delta_backpressure, 1);

    let air = empty(9);
    assert_eq!(air.cause, DiagnosticCause::AirLoss);
    assert_eq!(air.cause.layer(), DiagnosticLayer::TxAirtime);
    assert_eq!(air.dropped_frames, 0);

    let generator = empty(11);
    assert_eq!(generator.cause, DiagnosticCause::GeneratorMissed);
    assert_eq!(generator.cause.layer(), DiagnosticLayer::Generator);

    // Slots after the last record have no closing boundary.
    let trailing = empty(13);
    assert_eq!(trailing.cause, DiagnosticCause::Ambiguous);
    assert!(trailing.next.is_none());
    assert!(trailing.previous.is_some());
}

#[test]
fn second_statistics_are_exact() {
    // 10 slots per second: slot i starts at i * 100 ms.
    let origin = 1_000_000;
    let samples = [
        Sample::new(1, origin),
        Sample::new(2, origin + 100_000),
        Sample::new(3, origin + 200_000),
        // Slots 3 and 4 are empty.
        Sample::new(4, origin + 500_000),
        Sample::new(5, origin + 600_000),
        // Slot 7 is empty.
        Sample::new(6, origin + 800_000),
        Sample::new(7, origin + 900_000),
    ];
    let data = prepare(&samples, origin, 10.0);
    let trace = data.diagnostics.as_ref().expect("trace");
    let stats = &trace.seconds[0];

    assert_eq!(stats.second, 0);
    assert_eq!(stats.slots, 10);
    assert_eq!(stats.samples, 7);
    assert_eq!(stats.filled_slots, 7);
    assert_eq!(stats.empty_slots, 3);
    assert_eq!(stats.effective_pps, 7.0);
    assert!((stats.loss_ratio - 0.3).abs() < 1e-12);
    assert!((stats.nominal_interval_us - 100_000.0).abs() < 1e-9);

    // Intervals between consecutive records: 100, 100, 300, 100, 200, 100 ms.
    // The first record of the trace has no predecessor, so it contributes none.
    assert!((stats.interval_mean_us - 150_000.0).abs() < 1e-6);
    assert!((stats.interval_median_us - 100_000.0).abs() < 1e-9);
    assert_eq!(stats.interval_min_us, 100_000);
    assert_eq!(stats.interval_max_us, 300_000);
    assert!((stats.interval_p95_us - 300_000.0).abs() < 1e-9);
    let expected_std = {
        let mean = 150_000.0;
        let values: [f64; 6] = [100_000.0, 100_000.0, 300_000.0, 100_000.0, 200_000.0, 100_000.0];
        (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 6.0).sqrt()
    };
    assert!((stats.interval_std_us - expected_std).abs() < 1e-6);

    // One run of two empty slots and one run of one.
    assert_eq!(stats.burst_histogram, [1, 1, 0]);
    assert_eq!(stats.longest_empty_run, 2);

    assert_eq!(stats.offset_mean_us, 0.0);
    assert_eq!(stats.offset_min_us, 0);
    assert_eq!(stats.offset_max_us, 0);

    assert_eq!(stats.stream_gap_total, 0);
    assert_eq!(stats.drop_total, 0);
    assert_eq!(stats.backpressure_total, 0);
    assert_eq!(stats.backpressure_frames, 0);

    assert_eq!(stats.rssi_min_dbm, Some(-35));
    assert_eq!(stats.rssi_max_dbm, Some(-35));
    assert_eq!(stats.rssi_mean_dbm, Some(-35.0));
    assert_eq!(stats.empty_slots_low_rssi, 0);

    assert_eq!(stats.missing_fresh, 0);
    assert_eq!(stats.first_word_invalid, 0);
    assert_eq!(stats.config_changes, 0);
    assert!(!stats.partial);
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
