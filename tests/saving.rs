//! End-to-end test for the `.npz` export.
//!
//! The golden values come from `capture_v8.bin` and match the decoder's own
//! integration tests, so a regression in either the decoder or the array layout
//! is caught here.

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use ndarray::Array1;
use ndarray_npy::NpzReader;
use rt_wireless_sensing::saving::{SaveSummary, save_capture_to};

/// Temporary archive path, unique to this process.
fn temp_output() -> PathBuf {
    std::env::temp_dir().join(format!("rt-wireless-saving-{}.npz", std::process::id()))
}

#[test]
fn capture_becomes_129_arrays_with_relative_millisecond_timestamps() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("capture_v8.bin");
    let output = temp_output();

    let summary: SaveSummary =
        save_capture_to(&input, &output).expect("capture should convert to npz");

    assert_eq!(summary.records, 691);
    assert_eq!(summary.subcarriers, 64);
    assert_eq!(summary.arrays, 129);
    assert!((summary.duration_ms - 34_499.075).abs() < 1e-6);
    assert!((summary.pps - 20.0).abs() < 0.01);

    let mut npz = NpzReader::new(File::open(&output).expect("archive should exist"))
        .expect("archive should be readable");
    assert_eq!(npz.len(), 129);

    let names = npz.names().expect("names should be readable");
    assert_eq!(names.len(), 129);
    assert!(names.contains(&"timestamp_ms".to_string()));
    assert!(names.contains(&"amplitude_00".to_string()));
    assert!(names.contains(&"amplitude_63".to_string()));
    assert!(names.contains(&"phase_00".to_string()));
    assert!(names.contains(&"phase_63".to_string()));

    let timestamps: Array1<f64> = npz.by_name("timestamp_ms").expect("timestamp array");
    assert_eq!(timestamps.len(), 691);
    assert_eq!(timestamps[0], 0.0);
    assert!((timestamps[690] - 34_499.075).abs() < 1e-6);

    // Bin 4 is the first live subcarrier: (I, Q) = (-20, 5).
    let amplitude: Array1<f32> = npz.by_name("amplitude_04").expect("amplitude_04");
    let phase: Array1<f32> = npz.by_name("phase_04").expect("phase_04");
    assert_eq!(amplitude.len(), 691);
    assert_eq!(phase.len(), 691);
    assert!((amplitude[0] - 20.615_528).abs() < 1e-4);
    assert!((phase[0] - 2.896_614).abs() < 1e-4);

    // Bin 0 is a guard null in the centered HT20 layout.
    let null: Array1<f32> = npz.by_name("amplitude_00").expect("amplitude_00");
    assert!(null.iter().all(|&value| value == 0.0));

    drop(npz);
    fs::remove_file(&output).ok();
}

#[test]
fn derived_npz_path_keeps_the_capture_stem() {
    assert_eq!(
        rt_wireless_sensing::saving::npz_path_for(Path::new("capture_v8.bin")),
        PathBuf::from("capture_v8.npz")
    );
}
