//! Convert a collected ESPectre binary capture into numeric `.npz` arrays.
//!
//! The input is the raw byte stream written by [`crate::collection`]; the
//! output is a NumPy `.npz` archive next to it with the same file stem, for
//! example `capture_v8.bin` becomes `capture_v8.npz`.
//!
//! # Output layout
//!
//! One timestamp array plus one amplitude and one phase array per subcarrier:
//!
//! | Array | Shape | dtype | Meaning |
//! |---|---|---|---|
//! | `timestamp_ms` | `(N,)` | `f64` | Sample time in milliseconds, relative to the first record |
//! | `amplitude_00` … `amplitude_NN` | `(N,)` | `f32` | `hypot(I, Q)` per subcarrier |
//! | `phase_00` … `phase_NN` | `(N,)` | `f32` | `atan2(Q, I)` per subcarrier, radians |
//!
//! `N` is the number of decoded CSI records. With the usual 64-subcarrier
//! capture that is `1 + 64 + 64 = 129` arrays, all of length `N`.
//!
//! # Timestamps
//!
//! The raw wire format has no wall-clock field. The only clock attached to
//! every sample is the device's monotonic `device_ticks_us`, so timestamps are
//!
//! ```text
//! timestamp_ms[i] = (device_ticks_us[i] - device_ticks_us[0]) / 1000
//! ```
//!
//! This starts at exactly `0.0` for the first record and is unaffected by
//! network or decode timing. Records must have a non-zero, non-decreasing
//! `device_ticks_us`; otherwise the function returns an error instead of
//! writing misleading times.
//!
//! Phase is saved as the raw wrapped value in `[-π, π]` radians. No unwrapping,
//! smoothing, or null-subcarrier removal is applied.

use std::{
    error::Error,
    fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use ndarray::Array1;
use ndarray_npy::{NpzWriter, WriteNpzError};

use crate::decoder::{CsiPacket, DecodeError, FrameStreamDecoder};

/// Name of the timestamp array in the archive.
pub const TIMESTAMP_ARRAY: &str = "timestamp_ms";

/// Result of saving one capture.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveSummary {
    /// Input binary file.
    pub input: PathBuf,
    /// Written `.npz` file.
    pub output: PathBuf,
    /// Number of decoded CSI records (`N`).
    pub records: usize,
    /// Complex subcarriers per record.
    pub subcarriers: usize,
    /// Number of arrays in the archive: `1 + 2 * subcarriers`.
    pub arrays: usize,
    /// Timestamp of the last record in milliseconds.
    pub duration_ms: f64,
    /// Effective record rate over the capture, using device ticks.
    pub pps: f64,
    /// Size of the written `.npz` file in bytes.
    pub bytes_written: u64,
}

/// Failure modes of [`save_capture`] and [`save_capture_to`].
#[derive(Debug)]
pub enum SaveError {
    /// Reading the input or writing the archive failed.
    Io(io::Error),
    /// The binary capture is malformed or truncated.
    Decode(DecodeError),
    /// The capture contained no complete records.
    NoRecords,
    /// A record reported zero subcarriers.
    NoSubcarriers {
        /// Zero-based record index.
        record: usize,
    },
    /// A record's `device_ticks_us` was zero, so no relative time exists.
    MissingDeviceTicks {
        /// Zero-based record index.
        record: usize,
    },
    /// Device ticks moved backwards, which would produce non-monotonic times.
    NonMonotonicDeviceTicks {
        /// Zero-based record index.
        record: usize,
        /// Tick value of the previous record.
        previous: u64,
        /// Tick value of this record.
        found: u64,
    },
    /// The subcarrier count changed mid-capture.
    SubcarrierCountChanged {
        /// Zero-based record index.
        record: usize,
        /// Count taken from the first record.
        expected: usize,
        /// Count found in this record.
        found: usize,
    },
    /// The `.npz` writer failed.
    WriteNpz(WriteNpzError),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error while saving CSI: {error}"),
            Self::Decode(error) => write!(f, "cannot decode capture: {error}"),
            Self::NoRecords => write!(f, "capture contains no complete CSI records"),
            Self::NoSubcarriers { record } => {
                write!(f, "record {record} reports zero subcarriers")
            }
            Self::MissingDeviceTicks { record } => write!(
                f,
                "record {record} has device_ticks_us == 0; cannot build a relative timestamp"
            ),
            Self::NonMonotonicDeviceTicks {
                record,
                previous,
                found,
            } => write!(
                f,
                "record {record} moved device_ticks_us backwards: {previous} -> {found}"
            ),
            Self::SubcarrierCountChanged {
                record,
                expected,
                found,
            } => write!(
                f,
                "record {record} has {found} subcarriers, expected {expected} from the first record"
            ),
            Self::WriteNpz(error) => write!(f, "cannot write .npz archive: {error}"),
        }
    }
}

impl Error for SaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::WriteNpz(error) => Some(error),
            Self::NoRecords
            | Self::NoSubcarriers { .. }
            | Self::MissingDeviceTicks { .. }
            | Self::NonMonotonicDeviceTicks { .. }
            | Self::SubcarrierCountChanged { .. } => None,
        }
    }
}

impl From<io::Error> for SaveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DecodeError> for SaveError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<WriteNpzError> for SaveError {
    fn from(error: WriteNpzError) -> Self {
        Self::WriteNpz(error)
    }
}

/// Derive the `.npz` path for a capture: replace the extension, keep the stem.
///
/// `capture_v8.bin` becomes `capture_v8.npz`; a path without an extension gets
/// `.npz` appended.
pub fn npz_path_for(bin_path: &Path) -> PathBuf {
    bin_path.with_extension("npz")
}

/// Decode `bin_path` and write `npz_path_for(bin_path)`.
///
/// This is the one-argument convenience form of [`save_capture_to`].
pub fn save_capture(bin_path: &Path) -> Result<SaveSummary, SaveError> {
    save_capture_to(bin_path, &npz_path_for(bin_path))
}

/// Decode `bin_path` and write the arrays to `npz_path`.
///
/// Every record is decoded with [`FrameStreamDecoder`]; decoding stops on the
/// first malformed frame. The timestamp array is built from `device_ticks_us`
/// relative to the first record, as described in the [module docs](self).
///
/// # Errors
///
/// Returns [`SaveError`] when the input cannot be read or decoded, when timestamps
/// are missing or non-monotonic, when the subcarrier count changes, or when the
/// `.npz` writer fails.
pub fn save_capture_to(bin_path: &Path, npz_path: &Path) -> Result<SaveSummary, SaveError> {
    let bytes = std::fs::read(bin_path)?;
    let packets = decode_capture(&bytes)?;
    let (timestamps, amplitudes, phases, subcarriers) = build_arrays(&packets)?;

    let records = packets.len();
    let duration_ms = timestamps.last().copied().unwrap_or(0.0);
    let pps = if records > 1 && duration_ms > 0.0 {
        (records - 1) as f64 / (duration_ms / 1_000.0)
    } else {
        0.0
    };

    write_npz(npz_path, timestamps, amplitudes, phases)?;

    let bytes_written = std::fs::metadata(npz_path)?.len();

    Ok(SaveSummary {
        input: bin_path.to_path_buf(),
        output: npz_path.to_path_buf(),
        records,
        subcarriers,
        arrays: 1 + 2 * subcarriers,
        duration_ms,
        pps,
        bytes_written,
    })
}

/// Decode every complete frame in `bytes`.
///
/// Shared with [`crate::dsp`], which builds a [`crate::dsp::CsiSignal`] from the
/// same packets so there is exactly one decode path.
pub(crate) fn decode_capture(bytes: &[u8]) -> Result<Vec<CsiPacket>, SaveError> {
    let mut decoder = FrameStreamDecoder::with_capacity(bytes.len().max(1_024));
    decoder.push(bytes);

    let mut packets = Vec::new();
    while let Some(packet) = decoder.next_frame()? {
        packets.push(packet);
    }
    // Reject a truncated final frame or stray trailing bytes rather than
    // silently saving a partial capture.
    decoder.finish()?;

    Ok(packets)
}

/// Build per-subcarrier columns and the relative millisecond timestamps.
///
/// Returns `(timestamps, amplitudes, phases, subcarriers)` where both matrix
/// vectors are indexed as `[subcarrier][record]`.
#[allow(clippy::type_complexity)]
fn build_arrays(
    packets: &[CsiPacket],
) -> Result<(Vec<f64>, Vec<Vec<f32>>, Vec<Vec<f32>>, usize), SaveError> {
    let first = packets.first().ok_or(SaveError::NoRecords)?;
    let subcarriers = first.csi.num_sc as usize;
    if subcarriers == 0 {
        return Err(SaveError::NoSubcarriers { record: 0 });
    }

    let first_ticks = first.csi.device_ticks_us;
    if first_ticks == 0 {
        return Err(SaveError::MissingDeviceTicks { record: 0 });
    }

    let mut timestamps = Vec::with_capacity(packets.len());
    let mut amplitudes: Vec<Vec<f32>> = vec![Vec::with_capacity(packets.len()); subcarriers];
    let mut phases: Vec<Vec<f32>> = vec![Vec::with_capacity(packets.len()); subcarriers];
    let mut previous_ticks = first_ticks;

    for (record, packet) in packets.iter().enumerate() {
        let ticks = packet.csi.device_ticks_us;
        if ticks == 0 {
            return Err(SaveError::MissingDeviceTicks { record });
        }
        if ticks < previous_ticks {
            return Err(SaveError::NonMonotonicDeviceTicks {
                record,
                previous: previous_ticks,
                found: ticks,
            });
        }

        let found_subcarriers = packet.csi.num_sc as usize;
        if found_subcarriers != subcarriers {
            return Err(SaveError::SubcarrierCountChanged {
                record,
                expected: subcarriers,
                found: found_subcarriers,
            });
        }

        timestamps.push((ticks - first_ticks) as f64 / 1_000.0);

        let amplitude = packet.amplitudes();
        let phase = packet.phases();
        for subcarrier in 0..subcarriers {
            amplitudes[subcarrier].push(amplitude[subcarrier]);
            phases[subcarrier].push(phase[subcarrier]);
        }

        previous_ticks = ticks;
    }

    Ok((timestamps, amplitudes, phases, subcarriers))
}

/// Write the timestamp, amplitude, and phase arrays into one `.npz` archive.
fn write_npz(
    path: &Path,
    timestamps: Vec<f64>,
    amplitudes: Vec<Vec<f32>>,
    phases: Vec<Vec<f32>>,
) -> Result<(), SaveError> {
    let mut npz = NpzWriter::new(File::create(path)?);

    npz.add_array(TIMESTAMP_ARRAY, &Array1::from_vec(timestamps))?;

    for (subcarrier, values) in amplitudes.into_iter().enumerate() {
        npz.add_array(
            format!("amplitude_{subcarrier:02}"),
            &Array1::from_vec(values),
        )?;
    }

    for (subcarrier, values) in phases.into_iter().enumerate() {
        npz.add_array(format!("phase_{subcarrier:02}"), &Array1::from_vec(values))?;
    }

    npz.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npz_path_replaces_the_binary_extension() {
        assert_eq!(
            npz_path_for(Path::new("capture_v8.bin")),
            PathBuf::from("capture_v8.npz")
        );
        assert_eq!(
            npz_path_for(Path::new("data/capture.bin")),
            PathBuf::from("data/capture.npz")
        );
        assert_eq!(
            npz_path_for(Path::new("capture_v8")),
            PathBuf::from("capture_v8.npz")
        );
    }

    #[test]
    fn empty_capture_is_rejected() {
        assert!(matches!(build_arrays(&[]), Err(SaveError::NoRecords)));
    }
}
