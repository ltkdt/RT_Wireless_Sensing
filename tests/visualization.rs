//! End-to-end integration tests for the visualization module.
//!
//! Validates decoding and preparation of `capture_v8.bin` into the data structures
//! required for HTML Canvas 2D heatmap rendering.

use std::{fs, path::PathBuf};

use rt_wireless_sensing::visualization::{VisualizationData, VisualizationError};

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
