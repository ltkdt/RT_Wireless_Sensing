//! The capture fixtures the tests run against.
//!
//! `capture_v8.bin` lives in `samples/` and is committed, so a fresh clone can
//! run the whole test suite. This module is the single place that knows the
//! layout: unit tests in `src/` and integration tests in `tests/` both go
//! through [`capture_v8`] instead of joining `CARGO_MANIFEST_DIR` with a bare
//! file name.
//!
//! Keeping it in the library rather than in `tests/common` matters: a helper
//! under `tests/` is a separate crate and cannot be `use`d from unit tests
//! inside `src/`, and `visualization`'s unit tests need the same fixture.
//!
//! # The fixture
//!
//! `samples/capture_v8.bin` is an ESP32-C5 capture, channel 149, HT20, 64
//! subcarriers, **501 records over 4.999 s at 100 PPS**, no sequence gaps and
//! no dropped frames. Every golden number in the tests derives from it; if the
//! file is ever replaced, those numbers must move with it.
//!
//! # Example
//!
//! ```
//! use rt_wireless_sensing::fixtures::capture_v8;
//!
//! assert!(capture_v8().ends_with("samples/capture_v8.bin"));
//! ```

use std::path::PathBuf;

/// Directory holding the sample captures, relative to the crate root.
pub const SAMPLES_DIR: &str = "samples";

/// File name of the V8 capture the tests decode.
pub const CAPTURE_V8: &str = "capture_v8.bin";

/// Number of CSI records in [`capture_v8`], as locked by the decoder tests.
pub const CAPTURE_V8_FRAMES: usize = 501;

/// Duration of [`capture_v8`] in seconds, from the first to the last record.
pub const CAPTURE_V8_DURATION_SECONDS: f64 = 4.999_075;

/// The repository's `samples/` directory.
///
/// Resolved from `CARGO_MANIFEST_DIR`, which is the crate root for both unit and
/// integration tests, so the result does not depend on the directory the test
/// binary happens to run in.
pub fn samples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SAMPLES_DIR)
}

/// The `samples/capture_v8.bin` fixture.
///
/// The path is returned whether or not the file exists, so a caller reads it and
/// gets a normal I/O error naming the path if the fixture is missing.
pub fn capture_v8() -> PathBuf {
    samples_dir().join(CAPTURE_V8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_live_under_samples() {
        let path = capture_v8();
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(CAPTURE_V8));
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(SAMPLES_DIR)
        );
    }

    #[test]
    fn the_capture_fixture_is_present() {
        // Fails with the resolved path, so a missing fixture is actionable.
        let path = capture_v8();
        assert!(
            path.is_file(),
            "capture fixture missing at {}; samples/ must ship with the repository",
            path.display()
        );
    }

    #[test]
    fn the_capture_fixture_is_not_empty() {
        let len = std::fs::metadata(capture_v8())
            .expect("fixture metadata should be readable")
            .len();
        assert!(len > 0, "fixture is empty");
    }
}
