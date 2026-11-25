//! CSI Data Visualization module.
//!
//! Prepares decoded CSI records from [`crate::decoder`] for 2D amplitude heatmap
//! rendering and web dashboard display.
//!
//! # Architecture
//!
//! * **Data Preparation (Rust)**: Decodes the V8 byte stream, validates monotonic
//!   device clocks, extracts the $N \times 64$ amplitude matrix, and compiles
//!   measurement metadata.
//! * **Web Visualization (HTML5 / Canvas 2D)**: Pure HTML, CSS, and Vanilla
//!   JavaScript that renders an interactive, high-performance scientific heatmap
//!   with colormaps (Viridis, Plasma, Jet), temporal axes, colorbar, and
//!   measurement statistics.
//! * **Distribution**: Can export standalone single-file HTML dashboards, JSON
//!   data payloads, or serve the dashboard via a lightweight built-in HTTP server.

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{self, Write},
    path::Path,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::collection::{NominalPpsSource, TrafficGenerator};
use crate::csi_header::{RAW_CSI_FLAG_FIRST_WORD_INVALID, RAW_CSI_FLAG_FRESH};
use crate::decoder::{CsiPacket, DecodeError, FrameStreamDecoder};

/// Embedded dashboard HTML template.
pub const HTML_TEMPLATE: &str = include_str!("../web/index.html");

/// Embedded dashboard CSS stylesheet.
pub const CSS_STYLES: &str = include_str!("../web/style.css");

/// Embedded dashboard JavaScript visualizer.
pub const JS_VISUALIZER: &str = include_str!("../web/visualizer.js");

/// Measurement metadata for dashboard display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualizationMetadata {
    /// Title of the visualization.
    pub title: String,
    /// Optional source file name or descriptor.
    pub source: Option<String>,
    /// Device monotonic clock of the first sample in microseconds.
    pub start_time_us: u64,
    /// Human-readable formatted start time.
    pub start_time_formatted: String,
    /// Total duration in seconds from first to last sample.
    pub duration_seconds: f64,
    /// Total number of decoded CSI frames ($N$).
    pub total_frames: usize,
    /// Subcarriers per frame (typically 64).
    pub subcarriers: usize,
    /// Effective sample rate in packets per second.
    pub effective_pps: f64,
    /// Nominal (requested) packet rate, i.e. the `--pps` argument, used to
    /// divide a one-second interval into occupancy slots.
    ///
    /// `None` when no `--pps` argument was supplied: the dashboard then falls
    /// back to a default nominal rate and the metadata card says so. The
    /// measured rate is always available as
    /// [`VisualizationMetadata::effective_pps`].
    #[serde(default)]
    pub nominal_pps: Option<f64>,
    /// Where [`Self::nominal_pps`] came from.
    ///
    /// `None` when nothing declared a rate, or when the payload predates this
    /// field. Lets the dashboard tell a rate it may drive the occupancy grid
    /// with (`argument`) from one that describes the device's own cadence.
    #[serde(default)]
    pub nominal_pps_source: Option<NominalPpsSource>,
    /// Who produced the traffic this payload describes.
    ///
    /// `None` means unknown — an offline payload whose file name carries no
    /// suffix, or one built by a tool with no device to ask. The dashboard then
    /// renders the mode as `—` rather than guessing.
    #[serde(default)]
    pub traffic_mode: Option<TrafficGenerator>,
    /// Wi-Fi channel if available.
    pub channel: Option<u8>,
    /// Hex-formatted device identifier if available.
    pub device_id: Option<String>,
}

/// Placeholder structure for activity or gesture classification results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResult {
    /// Predicted activity or gesture label (e.g. "Walking", "Waving", "Standstill").
    pub predicted_class: Option<String>,
    /// Confidence score between 0.0 and 1.0.
    pub confidence: Option<f32>,
    /// Optional supplementary details or model identifier.
    pub details: Option<String>,
}

/// Diagnostic trace for the dashboard's diagnostic view (PPS-deficit analysis).
///
/// Every value comes from the `.bin` capture itself — the 60-byte HTTP frame
/// prefix and the 64-byte CSI record header — with no decoder change. See
/// `PLAN.md` ("Diagnostic View") for the design and the classification rules.
///
/// The trace is attached to [`VisualizationData`] as an `Option`, so payloads
/// produced before the diagnostic view existed deserialize with `None` and the
/// web UI keeps the toggle hidden.
/// RSSI at or below this value (dBm) counts as "low" when an empty slot is
/// classified as over-air loss.
///
/// The threshold is absolute because a capture carries no per-session
/// reference: a link that normally sits at −35 dBm and falls to −75 dBm around
/// an empty slot is the signature of a marker lost over the air.
pub const LOW_RSSI_THRESHOLD_DBM: i8 = -70;

/// Layer a diagnostic finding points at.
///
/// Every counter is recorded on the RX device, but each one attributes the
/// cause to a different layer; see `PLAN.md` §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLayer {
    /// The RX device (ESP32) never sent the record.
    RxDevice,
    /// The desktop client did not accept the batch.
    Desktop,
    /// The marker frame was lost over the air (AP → device).
    TxAirtime,
    /// The generator did not send the marker, or firmware produced no CSI.
    Generator,
    /// Not enough information to attribute the loss.
    Unknown,
}

/// Why a slot holds no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCause {
    /// `stream_sequence` gap with `raw_drop_total` rising: the device dropped
    /// the record before sending it (queue full, invalid packet, batch send
    /// failed). This is an **RX-side** drop, not a desktop receive problem.
    DeviceDrop,
    /// `stream_sequence` gap with `raw_send_backpressure_total` rising: the
    /// desktop did not accept the batch, so it reads too slowly.
    DesktopBackpressure,
    /// Contiguous counters and low neighbouring RSSI: the marker was lost over
    /// the air between the AP and the device.
    AirLoss,
    /// Contiguous counters and normal RSSI: no record was generated (marker not
    /// sent, or firmware produced no CSI).
    GeneratorMissed,
    /// No boundary record closes the gap (window or capture edge), so the loss
    /// cannot be attributed.
    Ambiguous,
}

impl DiagnosticCause {
    /// Layer this cause points at, when nothing is known about the traffic mode.
    ///
    /// This is the attribution for an external or unknown producer. When the
    /// device generated the traffic itself, use [`layer_for`], which moves
    /// [`Self::GeneratorMissed`] to [`DiagnosticLayer::RxDevice`].
    pub fn layer(self) -> DiagnosticLayer {
        match self {
            Self::DeviceDrop => DiagnosticLayer::RxDevice,
            Self::DesktopBackpressure => DiagnosticLayer::Desktop,
            Self::AirLoss => DiagnosticLayer::TxAirtime,
            Self::GeneratorMissed => DiagnosticLayer::Generator,
            Self::Ambiguous => DiagnosticLayer::Unknown,
        }
    }
}

/// Layer a cause points at, given who produced the traffic.
///
/// The classification itself is mode-independent, and is deliberately left
/// alone: a [`DiagnosticCause::GeneratorMissed`] slot really does have
/// contiguous drop counters and zero dropped frames, whether the packets were
/// missing from this process's marker loop or from the device's own generator.
/// Only the *attribution* changes.
///
/// * Internal mode → the device owns the generator, so the missing records come
///   from the device or its firmware: [`DiagnosticLayer::RxDevice`].
/// * Anything else → unchanged, i.e. [`DiagnosticCause::layer`].
pub fn layer_for(
    cause: DiagnosticCause,
    traffic_mode: Option<TrafficGenerator>,
) -> DiagnosticLayer {
    match (cause, traffic_mode) {
        (DiagnosticCause::GeneratorMissed, Some(TrafficGenerator::Internal)) => {
            DiagnosticLayer::RxDevice
        }
        (cause, _) => cause.layer(),
    }
}

/// One received record with every derived metric precomputed.
///
/// The web layer only formats these numbers; it keeps no counter state, so the
/// deltas are always between consecutively received frames (the counters are
/// session-cumulative).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    /// Index into [`VisualizationData::timestamps`] and `amplitudes`.
    pub frame: usize,
    /// Whole seconds since the trace origin.
    pub second: u32,
    /// Slot index inside `second`.
    pub slot: u32,
    /// `device_ticks_us − slot_start`, in µs; negative means early.
    pub slot_offset_us: i64,
    /// `stream_sequence`: one-based packets offered to the stream, including
    /// the ones the device dropped.
    pub stream_sequence: u64,
    /// Saturated copy of `stream_sequence` from the CSI record header.
    pub seq_num: u32,
    /// Device monotonic clock of the sample, in µs.
    pub device_ticks_us: u64,
    /// Δ`stream_sequence` against the previous record; `None` for the first.
    pub delta_stream: Option<u64>,
    /// Δ`raw_drop_total` against the previous record.
    pub delta_drop: Option<u64>,
    /// Δ`raw_send_backpressure_total` against the previous record.
    pub delta_backpressure: Option<u64>,
    /// Δ`device_ticks_us` against the previous record.
    pub delta_ticks_us: Option<u64>,
    /// Δ`wifi_rx_ts_us` against the previous record, when both are valid.
    ///
    /// The driver clock is 32-bit, so the difference is taken modulo `2^32`:
    /// a wrap shows as a small interval instead of a huge negative one.
    pub delta_wifi_rx_us: Option<i64>,
    /// RSSI in dBm.
    pub rssi_dbm: i8,
    /// Noise floor in dBm; `None` when the device sent the `-128` sentinel.
    pub noise_floor_dbm: Option<i8>,
    /// `rssi_dbm − noise_floor_dbm`, when the noise floor is known.
    pub snr_db: Option<i16>,
    /// Raw `flags` byte of the CSI record header.
    pub flags: u8,
    /// Wi-Fi channel.
    pub channel: u8,
    /// PHY mode code (see [`crate::csi_header::phy_mode_name`]).
    pub phy_mode_code: u8,
    /// Channel width code (see [`crate::csi_header::channel_width_name`]).
    pub channel_width_code: u8,
}

/// One record at the edge of an empty slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticBoundary {
    /// Index into [`VisualizationData::timestamps`].
    pub frame: usize,
    /// Saturated `stream_sequence` copy.
    pub seq_num: u32,
    /// Device monotonic clock, in µs.
    pub device_ticks_us: u64,
    /// RSSI in dBm.
    pub rssi_dbm: i8,
}

/// One slot without a record, with the cause inferred from its boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticEmptySlot {
    /// Whole seconds since the trace origin.
    pub second: u32,
    /// Slot index inside `second`.
    pub slot: u32,
    /// Inferred cause.
    pub cause: DiagnosticCause,
    /// Frames the stream skipped across the gap (`Δstream − 1`); `0` when the
    /// gap is unknown.
    pub dropped_frames: u64,
    /// Δ`raw_drop_total` across the gap.
    pub delta_drop: u64,
    /// Δ`raw_send_backpressure_total` across the gap.
    pub delta_backpressure: u64,
    /// Last record before the slot, when the trace contains one.
    pub previous: Option<DiagnosticBoundary>,
    /// First record after the slot, when the trace contains one.
    pub next: Option<DiagnosticBoundary>,
}

/// Per-second statistics for the diagnostic view.
///
/// Computed in Rust so the numbers can be exported for offline analysis; the
/// web layer only formats them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticSecondStats {
    /// Whole seconds since the trace origin.
    pub second: u32,
    /// Slot count of a full second.
    pub slots: usize,
    /// Records in this second.
    pub samples: usize,
    /// Slots holding at least one record.
    pub filled_slots: usize,
    /// Slots holding no record.
    pub empty_slots: usize,
    /// Records in this second (one second of device time).
    pub effective_pps: f64,
    /// `empty_slots / slots`.
    pub loss_ratio: f64,
    /// `1e6 / slots`, the theoretical interval between two records.
    pub nominal_interval_us: f64,
    /// Mean Δ`device_ticks_us` between consecutive records.
    pub interval_mean_us: f64,
    /// Median Δ`device_ticks_us`.
    pub interval_median_us: f64,
    /// Smallest Δ`device_ticks_us`.
    pub interval_min_us: u64,
    /// Largest Δ`device_ticks_us`.
    pub interval_max_us: u64,
    /// Population standard deviation of Δ`device_ticks_us` (jitter).
    pub interval_std_us: f64,
    /// 95th percentile of Δ`device_ticks_us` (nearest rank).
    pub interval_p95_us: f64,
    /// Number of empty-slot runs of length 1, 2, and 3 or more.
    pub burst_histogram: [usize; 3],
    /// Longest run of consecutive empty slots.
    pub longest_empty_run: usize,
    /// Mean slot offset (lateness) in µs.
    pub offset_mean_us: f64,
    /// Smallest slot offset in µs.
    pub offset_min_us: i64,
    /// Largest slot offset in µs.
    pub offset_max_us: i64,
    /// Σ(`Δstream − 1`): frames dropped at the device stream layer.
    pub stream_gap_total: u64,
    /// Σ`Δraw_drop_total`.
    pub drop_total: u64,
    /// Σ`Δraw_send_backpressure_total`.
    pub backpressure_total: u64,
    /// Records whose `Δraw_send_backpressure_total` is positive.
    pub backpressure_frames: usize,
    /// Lowest RSSI in the second.
    pub rssi_min_dbm: Option<i8>,
    /// Mean RSSI in the second.
    pub rssi_mean_dbm: Option<f64>,
    /// Highest RSSI in the second.
    pub rssi_max_dbm: Option<i8>,
    /// Empty slots classified as over-air loss.
    pub empty_slots_low_rssi: usize,
    /// Records without the `FRESH` flag.
    pub missing_fresh: usize,
    /// Records with the first-word-invalid flag.
    pub first_word_invalid: usize,
    /// Channel / PHY / width changes between consecutive records.
    pub config_changes: usize,
    /// `true` when the trace does not cover the whole second (first or last
    /// second of a capture or of a live window), so the loss ratio describes
    /// only the covered part.
    pub partial: bool,
}

/// Number of empty slots per [`DiagnosticCause`] over the whole capture.
///
/// Every empty slot between the first and the last received record is counted
/// exactly once, so the five fields sum to [`DiagnosticSummary::empty_slots`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DiagnosticCauseCounts {
    /// Empty slots attributed to the RX device (record never sent).
    pub device_drop: u64,
    /// Empty slots attributed to the desktop client (batch not accepted).
    pub desktop_backpressure: u64,
    /// Empty slots attributed to over-air loss (TX/airtime).
    pub air_loss: u64,
    /// Empty slots with no record generated (generator or firmware).
    pub generator_missed: u64,
    /// Empty slots no boundary record closes (capture or window edge).
    pub ambiguous: u64,
}

/// Capture-level aggregation of the diagnostic trace.
///
/// `total_slots` is the declared span of the run in slots, so it is the
/// denominator of every ratio here: `filled_slots + empty_slots` partition it,
/// and a run that ended early — or that never reached its own end — shows up as
/// empty slots instead of disappearing from the count.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DiagnosticSummary {
    /// Slots the run was declared to cover: `round(span × slots / 1e6)`.
    pub total_slots: u64,
    /// Slots that received at least one record.
    pub filled_slots: u64,
    /// Records received inside the span.
    pub filled_records: u64,
    /// `filled_records − filled_slots`: records that arrived in a slot another
    /// record already filled, i.e. less than one slot apart from each other.
    ///
    /// `#[serde(default)]` keeps payloads written before this field existed
    /// parseable; the web layer derives it from the other two when absent.
    #[serde(default)]
    pub extra_records: u64,
    /// Slots inside the span without a record.
    pub empty_slots: u64,
    /// `empty_slots / total_slots`; `0.0` for an empty trace.
    pub loss_ratio: f64,
    /// Empty slots grouped by cause.
    pub cause_counts: DiagnosticCauseCounts,
    /// Σ(`Δstream − 1`) over the empty slots: packets offered to the stream but
    /// never received.
    pub dropped_frames_total: u64,
    /// Σ`Δraw_drop_total` over the empty slots — RX-side drops.
    pub delta_drop_total: u64,
    /// Σ`Δraw_send_backpressure_total` over the empty slots — batches the
    /// desktop did not accept.
    pub delta_backpressure_total: u64,
    /// Empty slots classified as over-air loss (low neighbouring RSSI).
    pub empty_slots_low_rssi: u64,
}

/// Diagnostic trace for the dashboard's diagnostic view (PPS-deficit analysis).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticTrace {
    /// Integer slot count per second — the nominal PPS rounded once in Rust —
    /// used as the column count of the diagnostic slot grid. The web UI uses
    /// this explicit count instead of re-deriving it, so the grid matches the
    /// occupancy slot logic exactly.
    pub slots_per_second: usize,
    /// `device_ticks_us` that maps to timestamp `0.0`.
    pub origin_ticks: u64,
    /// `true` when the last record is the end of the capture, `false` when the
    /// trace ends at a live sliding-window edge.
    ///
    /// Streaming drops records outside its window before publishing, so without
    /// this flag every window edge would look like an unattributable gap.
    pub capture_end: bool,
    /// One row per received record, in capture order.
    pub records: Vec<DiagnosticRecord>,
    /// One row per slot without a record, in `(second, slot)` order.
    pub empty_slots: Vec<DiagnosticEmptySlot>,
    /// Per-second statistics, one row per second covered by the trace.
    pub seconds: Vec<DiagnosticSecondStats>,
    /// Capture-level aggregation of the empty slots, for the overview panel.
    ///
    /// `#[serde(default)]` keeps payloads written before this field existed
    /// parseable; the web layer falls back to aggregating `empty_slots` itself.
    #[serde(default)]
    pub summary: DiagnosticSummary,
}

/// One panel of the DSP view: a `[time][subcarrier]` matrix plus what the web
/// layer needs to render it (title, unit, colormap family).
///
/// The matrix is plain JSON (like [`VisualizationData::amplitudes`]) so the
/// payload stays readable in `--json` exports and needs no new dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DspPanel {
    /// Stable identifier: `"amplitude"`, `"phase"` or `"amplitude_norm"`.
    pub id: String,
    /// Human-readable title shown above the canvas.
    pub title: String,
    /// Value unit: `"a.u."`, `"rad"` or `"z"`.
    pub unit: String,
    /// Colormap family the web layer must use: `"sequential"`,
    /// `"cyclic"` (phase, wraps at ±π) or `"diverging"` (normalized, ±z).
    pub kind: String,
    /// `[time][subcarrier]` matrix, same shape as the resampled grid.
    pub data: Vec<Vec<f32>>,
}

/// Precomputed DSP view of a capture: resampling onto a uniform grid plus an
/// optional normalization, computed **once in Rust** when the payload is
/// prepared.
///
/// The mask is separable — gaps are classified per time column
/// ([`DspView::time_state`]) and null subcarriers per bin
/// ([`DspView::null_subcarriers`]) — so no per-cell mask matrix is shipped.
/// Streaming payloads carry `dsp: None`: DSP is never computed while a
/// dashboard is live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DspView {
    /// Interpolation used for the resampling: `"nearest"`, `"linear"` or
    /// `"cubic_spline"`.
    pub interp: String,
    /// Grid rate in Hz (the intended PPS), so column `j` maps to diagnostic
    /// slot `(second = j / target_fs, slot = j % target_fs)`.
    pub target_fs: f64,
    /// Applied normalization (`"per_subcarrier"`, `"per_sample"`, `"min_max"`),
    /// `None` when only resampling was asked for.
    pub norm: Option<String>,
    /// Gap threshold in milliseconds; `None` means every gap was bridged.
    pub max_gap_ms: Option<f64>,
    /// Resampled grid times in seconds from the first record.
    pub timestamps: Vec<f64>,
    /// Subcarrier indices that carry no channel information (guard/DC bins)
    /// and must be masked by the renderer.
    pub null_subcarriers: Vec<usize>,
    /// Per grid column: `0` observed, `1` bridged, `2` masked.
    pub time_state: Vec<u8>,
    /// Panels to draw, in order: `amplitude`, `phase`, then `amplitude_norm`
    /// when a normalization was applied.
    pub panels: Vec<DspPanel>,
}

/// Options that control how the DSP view is computed.
///
/// Every field is a plain DSP enum so [`VisualizationOptions`] stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DspOptions {
    /// Interpolation method for the resampling.
    pub interp: crate::dsp::InterpMethod,
    /// Normalization applied *after* the resampling, if any.
    pub norm: Option<crate::dsp::NormScheme>,
    /// Maximum bridged gap in milliseconds; longer gaps are masked. `None`
    /// (default) bridges every gap.
    pub max_gap_ms: Option<f64>,
}

/// Complete dataset prepared for the web visualization layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualizationData {
    /// Measurement metadata.
    pub metadata: VisualizationMetadata,
    /// Measurement time in seconds for each frame, starting at 0.0.
    pub timestamps: Vec<f64>,
    /// $N \times 64$ CSI amplitude matrix: `[frame_index][subcarrier_index]`.
    pub amplitudes: Vec<Vec<f32>>,
    /// Optional classification result (dedicated display area in dashboard).
    pub classification: Option<ClassificationResult>,
    /// Optional diagnostic trace for the diagnostic view.
    ///
    /// Like [`VisualizationData::classification`], this is an `Option`, so
    /// payloads produced before the diagnostic view existed deserialize with
    /// `None` and the web UI keeps the diagnostic toggle hidden.
    pub diagnostics: Option<DiagnosticTrace>,
    /// Optional precomputed DSP view (resampled and normalized panels).
    ///
    /// `#[serde(default)]` keeps payloads written before the DSP view existed
    /// parseable; the web layer then hides the DSP toggle. Live dashboard
    /// snapshots always carry `None`.
    #[serde(default)]
    pub dsp: Option<DspView>,
}

/// One state snapshot exposed by the live dashboard server.
///
/// The browser polls `/api/data` and receives either a waiting placeholder
/// (during the warm-up period or before the first record arrives) or a ready
/// [`VisualizationData`] payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LiveDashboardSnapshot {
    /// The dashboard is not ready to draw a heatmap yet.
    Waiting {
        /// Seconds elapsed since the streaming command started.
        elapsed_seconds: f64,
        /// Warm-up length requested by the streaming command.
        warmup_seconds: f64,
    },
    /// The current sliding-window dataset is ready.
    Ready {
        /// Data to render in the heatmap.
        data: Box<VisualizationData>,
    },
}

/// Errors that can occur during data preparation or visualization export.
#[derive(Debug)]
pub enum VisualizationError {
    /// File I/O failure.
    Io(io::Error),
    /// Decoding failure in the V8 parser.
    Decode(DecodeError),
    /// The input capture contained no complete CSI records.
    NoRecords,
    /// A record reported 0 subcarriers.
    NoSubcarriers {
        /// Zero-based record index.
        record: usize,
    },
    /// A record's `device_ticks_us` was 0.
    MissingDeviceTicks {
        /// Zero-based record index.
        record: usize,
    },
    /// Device ticks moved backwards.
    NonMonotonicDeviceTicks {
        /// Zero-based record index.
        record: usize,
        /// Previous ticks in microseconds.
        previous: u64,
        /// Current ticks in microseconds.
        found: u64,
    },
    /// A record predates the requested visualization time origin.
    TimestampBeforeOrigin {
        /// Zero-based record index.
        record: usize,
        /// Origin ticks in microseconds.
        origin: u64,
        /// Record ticks in microseconds.
        found: u64,
    },
    /// The subcarrier count changed mid-capture.
    SubcarrierCountChanged {
        /// Zero-based record index.
        record: usize,
        /// Expected subcarrier count.
        expected: usize,
        /// Found subcarrier count.
        found: usize,
    },
    /// JSON serialization failure.
    Serialization(serde_json::Error),
    /// The `dsp` module rejected a request while building the DSP view.
    Dsp(crate::dsp::DspError),
    /// Web server error.
    Server(String),
}

impl fmt::Display for VisualizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Decode(err) => write!(f, "decode error: {err}"),
            Self::NoRecords => write!(f, "capture contains no complete CSI records"),
            Self::NoSubcarriers { record } => {
                write!(f, "record {record} reports zero subcarriers")
            }
            Self::MissingDeviceTicks { record } => {
                write!(f, "record {record} has zero device_ticks_us")
            }
            Self::NonMonotonicDeviceTicks {
                record,
                previous,
                found,
            } => write!(
                f,
                "record {record} ticks moved backwards: previous={previous} us, found={found} us"
            ),
            Self::TimestampBeforeOrigin {
                record,
                origin,
                found,
            } => write!(
                f,
                "record {record} ticks {found} us are before the visualization origin {origin} us"
            ),
            Self::SubcarrierCountChanged {
                record,
                expected,
                found,
            } => write!(
                f,
                "record {record} subcarrier count changed: expected {expected}, found {found}"
            ),
            Self::Serialization(err) => write!(f, "JSON serialization error: {err}"),
            Self::Dsp(err) => write!(f, "DSP error while building the DSP view: {err}"),
            Self::Server(msg) => write!(f, "web server error: {msg}"),
        }
    }
}

impl Error for VisualizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Decode(err) => Some(err),
            Self::Serialization(err) => Some(err),
            Self::Dsp(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for VisualizationError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<DecodeError> for VisualizationError {
    fn from(err: DecodeError) -> Self {
        Self::Decode(err)
    }
}

impl From<crate::dsp::DspError> for VisualizationError {
    fn from(err: crate::dsp::DspError) -> Self {
        Self::Dsp(err)
    }
}

impl From<serde_json::Error> for VisualizationError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err)
    }
}

/// Decode raw bytes from a `.bin` capture into a list of [`CsiPacket`].
pub fn decode_packets(bytes: &[u8]) -> Result<Vec<CsiPacket>, VisualizationError> {
    let mut decoder = FrameStreamDecoder::with_capacity(bytes.len().max(1_024));
    decoder.push(bytes);

    let mut packets = Vec::new();
    while let Some(packet) = decoder.next_frame()? {
        packets.push(packet);
    }
    decoder.finish()?;
    Ok(packets)
}

/// Build the diagnostic trace for `packets`.
///
/// `origin_ticks` is the `device_ticks_us` that maps to timestamp `0.0`, so the
/// slot grid shares the time base of [`VisualizationData::timestamps`].
/// # Counting slots
///
/// `span_ticks` is how far the run is declared to last: the recording span of a
/// finished capture, or `latest − origin` for a live window. It replaces the
/// old "first to last record" span, which could only ever describe what arrived
/// and so could never show a deficit at the end of a run.
///
/// `total_slots` is that span rounded **once** to whole slots
/// (`round(span_seconds × slots_per_second)`), so `5.006366 s` at 100 PPS is
/// 501 slots and `5 s` is 500. `filled_slots` counts the slots that received at
/// least one record, which is why it can be smaller than the record count:
/// records that land in an already-filled slot are *extra*, not coverage.
///
/// `slots_per_second` is the nominal PPS rounded once by the caller.
///
/// Slot assignment uses integer arithmetic on device ticks
/// (`slot = within_second_us * slots / 1_000_000`), which is the exact form of
/// the web occupancy formula `floor((t − second_start) * slots + 1e-6)`.
///
/// The trace is deliberately mode-blind: it records the evidence (cause,
/// deltas, counters) and nothing about who produced the traffic. Attribution is
/// [`layer_for`]'s job, applied when the payload is read, so an internal and an
/// external capture of the same evidence serialize identically.
fn build_diagnostic_trace(
    packets: &[CsiPacket],
    origin_ticks: u64,
    slots_per_second: usize,
    capture_end: bool,
    span_ticks: u64,
) -> DiagnosticTrace {
    let slots_per_second = slots_per_second.max(1);
    let total_slots = slots_in_span(span_ticks, slots_per_second);
    let mut records = Vec::with_capacity(packets.len());
    let mut previous: Option<&CsiPacket> = None;

    for (frame, packet) in packets.iter().enumerate() {
        let ticks = packet.csi.device_ticks_us;
        let relative = ticks.saturating_sub(origin_ticks);
        let second = (relative / 1_000_000) as u32;
        let within = relative % 1_000_000;
        let slot = (within * slots_per_second as u64 / 1_000_000) as u32;
        let slot_start = u64::from(slot) * 1_000_000 / slots_per_second as u64;
        let slot_offset_us = within as i64 - slot_start as i64;

        let delta_stream = previous.map(|previous| {
            packet
                .frame
                .stream_sequence
                .saturating_sub(previous.frame.stream_sequence)
        });
        let delta_drop = previous.map(|previous| {
            packet
                .frame
                .raw_drop_total
                .saturating_sub(previous.frame.raw_drop_total)
        });
        let delta_backpressure = previous.map(|previous| {
            packet
                .frame
                .raw_send_backpressure_total
                .saturating_sub(previous.frame.raw_send_backpressure_total)
        });
        let delta_ticks_us =
            previous.map(|previous| ticks.saturating_sub(previous.csi.device_ticks_us));
        let delta_wifi_rx_us = previous.and_then(|previous| {
            match (
                packet.csi.valid_wifi_rx_ts_us(),
                previous.csi.valid_wifi_rx_ts_us(),
            ) {
                (Some(current), Some(previous)) => {
                    Some(i64::from(current.wrapping_sub(previous) as i32))
                }
                _ => None,
            }
        });

        let noise_floor_dbm = packet.csi.valid_noise_floor_dbm();
        records.push(DiagnosticRecord {
            frame,
            second,
            slot,
            slot_offset_us,
            stream_sequence: packet.frame.stream_sequence,
            seq_num: packet.csi.seq_num,
            device_ticks_us: ticks,
            delta_stream,
            delta_drop,
            delta_backpressure,
            delta_ticks_us,
            delta_wifi_rx_us,
            rssi_dbm: packet.csi.rssi_dbm,
            noise_floor_dbm,
            snr_db: noise_floor_dbm.map(|floor| i16::from(packet.csi.rssi_dbm) - i16::from(floor)),
            flags: packet.csi.flags,
            channel: packet.csi.channel,
            phy_mode_code: packet.csi.phy_mode_code,
            channel_width_code: packet.csi.channel_width_code,
        });
        previous = Some(packet);
    }

    let empty_slots = collect_empty_slots(&records, slots_per_second, total_slots);
    let seconds = collect_second_stats(&records, &empty_slots, slots_per_second);
    let summary = collect_summary(&records, &empty_slots, total_slots);

    DiagnosticTrace {
        slots_per_second,
        origin_ticks,
        capture_end,
        records,
        empty_slots,
        seconds,
        summary,
    }
}

/// Aggregate every empty slot of the trace into one [`DiagnosticSummary`].
///
/// `total_slots` comes from the declared span, not from the trace:
/// `round(span_ticks × slots / 1e6)`. The two counts that partition it are
/// `filled_slots` (slots that received a record) and `empty_slots`, so
/// `loss_ratio` describes the run the operator asked for, including a deficit
/// that no record would ever reveal. Records beyond the first in a slot are
/// reported separately as [`DiagnosticSummary::extra_records`].
fn collect_summary(
    records: &[DiagnosticRecord],
    empty_slots: &[DiagnosticEmptySlot],
    total_slots: u64,
) -> DiagnosticSummary {
    let mut cause_counts = DiagnosticCauseCounts::default();
    let mut dropped_frames_total = 0u64;
    let mut delta_drop_total = 0u64;
    let mut delta_backpressure_total = 0u64;
    let mut empty_slots_low_rssi = 0u64;

    for empty in empty_slots {
        let counter = match empty.cause {
            DiagnosticCause::DeviceDrop => &mut cause_counts.device_drop,
            DiagnosticCause::DesktopBackpressure => &mut cause_counts.desktop_backpressure,
            DiagnosticCause::AirLoss => &mut cause_counts.air_loss,
            DiagnosticCause::GeneratorMissed => &mut cause_counts.generator_missed,
            DiagnosticCause::Ambiguous => &mut cause_counts.ambiguous,
        };
        *counter += 1;

        dropped_frames_total += empty.dropped_frames;
        delta_drop_total += empty.delta_drop;
        delta_backpressure_total += empty.delta_backpressure;
        if empty.cause == DiagnosticCause::AirLoss {
            empty_slots_low_rssi += 1;
        }
    }

    let filled_records = records.len() as u64;
    let filled_slots = count_filled_slots(records);
    let empty_count = empty_slots.len() as u64;
    let total_slots = total_slots.max(filled_slots + empty_count);
    let loss_ratio = if total_slots == 0 {
        0.0
    } else {
        empty_count as f64 / total_slots as f64
    };

    DiagnosticSummary {
        total_slots,
        filled_slots,
        filled_records,
        extra_records: filled_records.saturating_sub(filled_slots),
        empty_slots: empty_count,
        loss_ratio,
        cause_counts,
        dropped_frames_total,
        delta_drop_total,
        delta_backpressure_total,
        empty_slots_low_rssi,
    }
}

/// Number of slots of `slots` columns that a span of `span_ticks` covers.
///
/// The span is converted once, in integer arithmetic, so the result does not
/// depend on floating-point rounding: `round(ticks × slots / 1e6)` is
/// `(ticks × slots + 500_000) / 1_000_000` for non-negative values.
fn slots_in_span(span_ticks: u64, slots: usize) -> u64 {
    let numerator = u128::from(span_ticks) * slots as u128 + 500_000;
    u64::try_from(numerator / 1_000_000).unwrap_or(u64::MAX)
}

/// Count the slots that hold at least one record.
///
/// Records are sorted by `(second, slot)`, so a slot is filled exactly when it
/// differs from the record before it.
fn count_filled_slots(records: &[DiagnosticRecord]) -> u64 {
    let mut filled = 0u64;
    let mut previous: Option<(u32, u32)> = None;

    for record in records {
        let key = (record.second, record.slot);
        if previous != Some(key) {
            filled += 1;
            previous = Some(key);
        }
    }
    filled
}

/// Enumerate every slot without a record between the first and last record.
///
/// The range stops at the last slot the span covers, so a capture never
/// reports a slot that lies past the point it is declared to end at.
fn collect_empty_slots(
    records: &[DiagnosticRecord],
    slots: usize,
    total_slots: u64,
) -> Vec<DiagnosticEmptySlot> {
    let (Some(first), Some(last)) = (records.first(), records.last()) else {
        return Vec::new();
    };

    let mut empty_slots = Vec::new();
    for second in first.second..=last.second {
        // Second `n` starts at slot `n × slots` of the span and owns `slots`
        // columns; the last one may be cut short by the end of the span.
        let first_slot = if second == first.second {
            first.slot
        } else {
            0
        };
        let base = u64::from(second) * slots as u64;
        let end_slot = total_slots.saturating_sub(base).min(slots as u64);

        for slot in first_slot..end_slot as u32 {
            // Records are sorted by (second, slot), so the first record at or
            // after this slot is the closing boundary and the one before it is
            // the opening boundary.
            let next_index =
                records.partition_point(|record| (record.second, record.slot) < (second, slot));
            if records
                .get(next_index)
                .is_some_and(|record| record.second == second && record.slot == slot)
            {
                continue;
            }

            let previous = next_index.checked_sub(1).map(|index| &records[index]);
            let next = records.get(next_index);
            let (cause, dropped_frames, delta_drop, delta_backpressure) =
                classify_empty_slot(previous, next);

            empty_slots.push(DiagnosticEmptySlot {
                second,
                slot,
                cause,
                dropped_frames,
                delta_drop,
                delta_backpressure,
                previous: previous.map(boundary),
                next: next.map(boundary),
            });
        }
    }
    empty_slots
}

/// Classify one empty slot from the records on both sides of it.
///
/// The decoder enforces `Δfresh_record_total == 1` between consecutively
/// received frames, so a stream gap is exactly `Δstream_sequence > 1`.
fn classify_empty_slot(
    previous: Option<&DiagnosticRecord>,
    next: Option<&DiagnosticRecord>,
) -> (DiagnosticCause, u64, u64, u64) {
    let (Some(previous), Some(next)) = (previous, next) else {
        return (DiagnosticCause::Ambiguous, 0, 0, 0);
    };

    let delta_stream = next
        .stream_sequence
        .saturating_sub(previous.stream_sequence);
    let delta_drop = next.delta_drop.unwrap_or(0);
    let delta_backpressure = next.delta_backpressure.unwrap_or(0);

    if delta_stream > 1 {
        let dropped_frames = delta_stream - 1;
        let cause = if delta_drop > 0 {
            DiagnosticCause::DeviceDrop
        } else if delta_backpressure > 0 {
            DiagnosticCause::DesktopBackpressure
        } else {
            // A gap the counters do not explain: do not guess a layer.
            DiagnosticCause::Ambiguous
        };
        (cause, dropped_frames, delta_drop, delta_backpressure)
    } else {
        let low_rssi = previous.rssi_dbm.min(next.rssi_dbm) <= LOW_RSSI_THRESHOLD_DBM;
        let cause = if low_rssi {
            DiagnosticCause::AirLoss
        } else {
            DiagnosticCause::GeneratorMissed
        };
        (cause, 0, delta_drop, delta_backpressure)
    }
}

/// Boundary summary of one record.
fn boundary(record: &DiagnosticRecord) -> DiagnosticBoundary {
    DiagnosticBoundary {
        frame: record.frame,
        seq_num: record.seq_num,
        device_ticks_us: record.device_ticks_us,
        rssi_dbm: record.rssi_dbm,
    }
}

/// Compute one [`DiagnosticSecondStats`] row per second covered by the trace.
fn collect_second_stats(
    records: &[DiagnosticRecord],
    empty_slots: &[DiagnosticEmptySlot],
    slots: usize,
) -> Vec<DiagnosticSecondStats> {
    let (Some(first), Some(last)) = (records.first(), records.last()) else {
        return Vec::new();
    };

    let mut stats = Vec::new();
    let mut record_start = 0;
    let mut empty_start = 0;

    for second in first.second..=last.second {
        let mut record_end = record_start;
        while record_end < records.len() && records[record_end].second == second {
            record_end += 1;
        }
        let mut empty_end = empty_start;
        while empty_end < empty_slots.len() && empty_slots[empty_end].second == second {
            empty_end += 1;
        }

        let second_records = &records[record_start..record_end];
        let partial = (second == first.second
            && second_records.first().is_some_and(|record| record.slot > 0))
            || (second == last.second
                && second_records
                    .last()
                    .is_some_and(|record| record.slot + 1 < slots as u32));

        stats.push(second_stats(
            second,
            second_records,
            &empty_slots[empty_start..empty_end],
            slots,
            partial,
        ));

        record_start = record_end;
        empty_start = empty_end;
    }
    stats
}

/// Statistics of one second.
fn second_stats(
    second: u32,
    records: &[DiagnosticRecord],
    empty_slots: &[DiagnosticEmptySlot],
    slots: usize,
    partial: bool,
) -> DiagnosticSecondStats {
    let samples = records.len();

    // Records are sorted by slot, so distinct slots are slot transitions.
    let mut filled_slots = 0;
    let mut previous_slot = None;
    for record in records {
        if previous_slot != Some(record.slot) {
            filled_slots += 1;
            previous_slot = Some(record.slot);
        }
    }

    let intervals: Vec<f64> = records
        .iter()
        .filter_map(|record| record.delta_ticks_us)
        .map(|delta| delta as f64)
        .collect();
    let mut sorted_intervals = intervals.clone();
    sorted_intervals
        .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let interval_mean_us = mean(&intervals);

    let offsets: Vec<f64> = records
        .iter()
        .map(|record| record.slot_offset_us as f64)
        .collect();

    let mut burst_histogram = [0usize; 3];
    let mut longest_empty_run = 0;
    let mut run = 0;
    let mut previous_empty_slot: Option<u32> = None;
    for empty in empty_slots {
        if previous_empty_slot.is_some_and(|slot| empty.slot == slot + 1) {
            run += 1;
        } else {
            if run > 0 {
                burst_histogram[(run - 1).min(2)] += 1;
                longest_empty_run = longest_empty_run.max(run);
            }
            run = 1;
        }
        previous_empty_slot = Some(empty.slot);
    }
    if run > 0 {
        burst_histogram[(run - 1).min(2)] += 1;
        longest_empty_run = longest_empty_run.max(run);
    }

    let empty_count = slots.saturating_sub(filled_slots);

    DiagnosticSecondStats {
        second,
        slots,
        samples,
        filled_slots,
        empty_slots: empty_count,
        effective_pps: samples as f64,
        loss_ratio: empty_count as f64 / slots as f64,
        nominal_interval_us: 1_000_000.0 / slots as f64,
        interval_mean_us,
        interval_median_us: median(&sorted_intervals),
        interval_min_us: sorted_intervals.first().copied().unwrap_or(0.0) as u64,
        interval_max_us: sorted_intervals.last().copied().unwrap_or(0.0) as u64,
        interval_std_us: std_dev(&intervals, interval_mean_us),
        interval_p95_us: percentile(&sorted_intervals, 0.95),
        burst_histogram,
        longest_empty_run,
        offset_mean_us: mean(&offsets),
        offset_min_us: records
            .iter()
            .map(|record| record.slot_offset_us)
            .min()
            .unwrap_or(0),
        offset_max_us: records
            .iter()
            .map(|record| record.slot_offset_us)
            .max()
            .unwrap_or(0),
        stream_gap_total: records
            .iter()
            .filter_map(|record| record.delta_stream)
            .map(|delta| delta.saturating_sub(1))
            .sum(),
        drop_total: records.iter().filter_map(|record| record.delta_drop).sum(),
        backpressure_total: records
            .iter()
            .filter_map(|record| record.delta_backpressure)
            .sum(),
        backpressure_frames: records
            .iter()
            .filter(|record| record.delta_backpressure.is_some_and(|delta| delta > 0))
            .count(),
        rssi_min_dbm: records.iter().map(|record| record.rssi_dbm).min(),
        rssi_mean_dbm: (samples > 0).then(|| {
            records
                .iter()
                .map(|record| f64::from(record.rssi_dbm))
                .sum::<f64>()
                / samples as f64
        }),
        rssi_max_dbm: records.iter().map(|record| record.rssi_dbm).max(),
        empty_slots_low_rssi: empty_slots
            .iter()
            .filter(|empty| empty.cause == DiagnosticCause::AirLoss)
            .count(),
        missing_fresh: records
            .iter()
            .filter(|record| record.flags & RAW_CSI_FLAG_FRESH == 0)
            .count(),
        first_word_invalid: records
            .iter()
            .filter(|record| record.flags & RAW_CSI_FLAG_FIRST_WORD_INVALID != 0)
            .count(),
        config_changes: records
            .windows(2)
            .filter(|pair| {
                pair[0].channel != pair[1].channel
                    || pair[0].phy_mode_code != pair[1].phy_mode_code
                    || pair[0].channel_width_code != pair[1].channel_width_code
            })
            .count(),
        partial,
    }
}

/// Arithmetic mean; `0.0` for an empty slice.
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Median of an already sorted slice; `0.0` when empty.
fn median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// Nearest-rank percentile of an already sorted slice; `0.0` when empty.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Population standard deviation; `0.0` for an empty slice.
fn std_dev(values: &[f64], mean: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

/// Options that control how [`VisualizationData`] is prepared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisualizationOptions {
    /// Nominal PPS used to divide a second into occupancy slots.
    ///
    /// `None` (or a non-positive value) falls back to the measured effective
    /// rate, which is wrong exactly for the seconds that miss samples — pass
    /// the configured `--pps` when preparing an offline file.
    pub nominal_pps: Option<f64>,
    /// Where [`Self::nominal_pps`] came from, copied into the metadata.
    ///
    /// `None` for payloads with no provenance: offline runs and live sessions
    /// that never resolved a rate.
    pub nominal_pps_source: Option<NominalPpsSource>,
    /// Who produced the traffic, copied into the metadata.
    ///
    /// `None` means unknown. `Some(TrafficGenerator::Internal)` also relabels
    /// the `generator_missed` diagnostic cause (see [`layer_for`]).
    pub traffic_mode: Option<TrafficGenerator>,
    /// `true` when `packets` ends at the end of the capture.
    ///
    /// A live dashboard publishes a sliding window, so its last record is not
    /// the end of the capture; the diagnostic trace uses this flag to tell a
    /// window edge from a capture edge.
    pub capture_end: bool,
    /// `true` to attach the diagnostic trace to the payload.
    ///
    /// The trace costs roughly 1 MB per 34 s of capture at 100 slots, so a live
    /// dashboard keeps it off by default and only builds it when the user asks
    /// for it. Offline payloads leave this at `true`. When it is `false`,
    /// `VisualizationData::diagnostics` is `None` and the web UI hides the
    /// diagnostic toggle; the occupancy readout is unaffected because it uses
    /// the timestamps, not the trace.
    pub diagnostics: bool,
    /// `Some(options)` to attach the precomputed DSP view.
    ///
    /// Offline payloads default to `Some(DspOptions::default())`: the analysis
    /// runs once, so it is exempt from the project-wide opt-in rule (streaming
    /// never computes it). `--no-dsp` sets this to `None` to keep the payload
    /// small; a live dashboard always uses `None`.
    pub dsp: Option<DspOptions>,
    /// Declared span of the run in device microseconds, when it is known
    /// independently of the records.
    ///
    /// The diagnostic summary divides this span into slots to get
    /// `total_slots`. `None` means "as far as the records reach", which is
    /// correct for an unlimited capture (the file ends at its last record) and
    /// for a live window (it counts up to the newest published record).
    ///
    /// A `--duration` capture does not need it either: its file is cut at the
    /// limit (see [`crate::collection`]), so `last − origin` already is the
    /// span. Pass it only when the records deliberately stop before the span,
    /// which is what happens when a shorter `--window` is served from a longer
    /// recording.
    pub span_us: Option<u64>,
}

impl Default for VisualizationOptions {
    fn default() -> Self {
        Self {
            nominal_pps: None,
            nominal_pps_source: None,
            traffic_mode: None,
            capture_end: true,
            diagnostics: true,
            dsp: Some(DspOptions::default()),
            span_us: None,
        }
    }
}

/// `device_ticks_us` of the first packet, or `0` when there is none.
fn first_device_ticks(packets: &[CsiPacket]) -> u64 {
    packets
        .first()
        .map_or(0, |packet| packet.csi.device_ticks_us)
}

/// Prepare [`VisualizationData`] from a slice of decoded [`CsiPacket`].
///
/// Timestamps start at `0.0` at the first packet's `device_ticks_us`.
pub fn prepare_visualization_data(
    packets: &[CsiPacket],
    source_name: Option<&str>,
) -> Result<VisualizationData, VisualizationError> {
    prepare_visualization_data_with_options(
        packets,
        source_name,
        first_device_ticks(packets),
        VisualizationOptions::default(),
    )
}

/// Prepare [`VisualizationData`] relative to an explicit time origin.
///
/// `origin_ticks` is the `device_ticks_us` value that maps to timestamp `0.0`.
/// Passing the first packet of the whole capture while `packets` contains only
/// a recent window keeps the window's timestamps at their absolute elapsed
/// values (for example, a 30-second window at 32 seconds starts at `2.0`).
///
/// `nominal_pps` is stored in the metadata for the web occupancy calculation.
/// When it is `None` or not positive, the effective PPS measured from the
/// packets is used instead.
pub fn prepare_visualization_data_with_origin(
    packets: &[CsiPacket],
    source_name: Option<&str>,
    origin_ticks: u64,
    nominal_pps: Option<f64>,
) -> Result<VisualizationData, VisualizationError> {
    prepare_visualization_data_with_options(
        packets,
        source_name,
        origin_ticks,
        VisualizationOptions {
            nominal_pps,
            capture_end: true,
            ..VisualizationOptions::default()
        },
    )
}

/// Prepare [`VisualizationData`] with explicit [`VisualizationOptions`].
pub fn prepare_visualization_data_with_options(
    packets: &[CsiPacket],
    source_name: Option<&str>,
    origin_ticks: u64,
    options: VisualizationOptions,
) -> Result<VisualizationData, VisualizationError> {
    let first = packets.first().ok_or(VisualizationError::NoRecords)?;
    let subcarriers = first.csi.num_sc as usize;
    if subcarriers == 0 {
        return Err(VisualizationError::NoSubcarriers { record: 0 });
    }

    if origin_ticks == 0 {
        return Err(VisualizationError::MissingDeviceTicks { record: 0 });
    }

    let mut timestamps = Vec::with_capacity(packets.len());
    let mut amplitudes = Vec::with_capacity(packets.len());
    let mut previous_ticks = origin_ticks;
    let mut last_ticks = origin_ticks;

    for (record, packet) in packets.iter().enumerate() {
        let ticks = packet.csi.device_ticks_us;
        if ticks == 0 {
            return Err(VisualizationError::MissingDeviceTicks { record });
        }
        if ticks < origin_ticks {
            return Err(VisualizationError::TimestampBeforeOrigin {
                record,
                origin: origin_ticks,
                found: ticks,
            });
        }
        if ticks < previous_ticks {
            return Err(VisualizationError::NonMonotonicDeviceTicks {
                record,
                previous: previous_ticks,
                found: ticks,
            });
        }
        let sc = packet.csi.num_sc as usize;
        if sc != subcarriers {
            return Err(VisualizationError::SubcarrierCountChanged {
                record,
                expected: subcarriers,
                found: sc,
            });
        }

        // Relative timestamp in seconds from the requested origin.
        let t_sec = (ticks - origin_ticks) as f64 / 1_000_000.0;
        timestamps.push(t_sec);
        amplitudes.push(packet.amplitudes());
        previous_ticks = ticks;
        last_ticks = ticks;
    }

    let total_frames = packets.len();
    let duration_seconds = timestamps.last().copied().unwrap_or(0.0);
    let effective_pps = if total_frames > 1 && duration_seconds > 0.0 {
        (total_frames - 1) as f64 / duration_seconds
    } else {
        0.0
    };
    // The requested rate is kept as-is: the dashboard displays it next to the
    // measured one, and "no --pps argument" is reported as `None` rather than
    // being silently replaced by the measurement.
    let nominal_pps = options
        .nominal_pps
        .filter(|value| value.is_finite() && *value > 0.0);
    // Slot grid (occupancy, diagnostics, DSP) needs a concrete rate; fall back
    // to the measured rate when no `--pps` was given.
    let slot_pps = nominal_pps.unwrap_or(effective_pps);

    let channel = Some(first.csi.channel);
    let device_id = Some(format!("0x{:08x}", first.csi.device_id));

    let metadata = VisualizationMetadata {
        title: "CSI Amplitude Heatmap".to_string(),
        source: source_name.map(|s| s.to_string()),
        start_time_us: origin_ticks,
        start_time_formatted: format!("0.000 s (Clock: {origin_ticks} µs)"),
        duration_seconds,
        total_frames,
        subcarriers,
        effective_pps,
        nominal_pps,
        nominal_pps_source: options.nominal_pps_source,
        traffic_mode: options.traffic_mode,
        channel,
        device_id,
    };

    // The diagnostic toggle is gated on the presence of this field, and the
    // slot-grid column count is passed explicitly (nominal PPS rounded once
    // here) so the web UI does not re-derive it. Building the trace is opt-out:
    // a live dashboard leaves it disabled to keep the payload small.
    //
    // The span decides `total_slots`. A finished capture declares it up front
    // (`--duration`, or the recording span when the run was unlimited); a live
    // window only knows how far it has got, so it counts up to the newest
    // record it publishes. Both cases fall back to `last − origin`, which is
    // exactly what an unrecorded `--duration` means on a finished capture.
    let span_ticks = options
        .span_us
        .unwrap_or_else(|| last_ticks.saturating_sub(origin_ticks));
    let diagnostics = options.diagnostics.then(|| {
        let slots_per_second = slot_pps.round().max(1.0) as usize;
        build_diagnostic_trace(
            packets,
            origin_ticks,
            slots_per_second,
            options.capture_end,
            span_ticks,
        )
    });

    // The DSP view is computed once, here, from the same packets as the
    // heatmap: Rust stays the only implementation of the formulas and the web
    // layer only formats what it receives. The grid rate is the intended PPS,
    // so resampled column `j` lines up with diagnostic slot `j`.
    let dsp = match options.dsp {
        Some(dsp_options) => Some(build_dsp_view(packets, slot_pps, dsp_options)?),
        None => None,
    };

    Ok(VisualizationData {
        metadata,
        timestamps,
        amplitudes,
        classification: None,
        diagnostics,
        dsp,
    })
}

/// Build the [`DspView`] payload: resample onto the intended-PPS grid, then
/// optionally normalize.
///
/// Panels shipped, in order: resampled `amplitude` and `phase`; then
/// `amplitude_norm` when a normalization was requested (phase is left alone by
/// [`crate::dsp::CsiSignal::normalize`], so it has no normalized counterpart).
fn build_dsp_view(
    packets: &[CsiPacket],
    nominal_pps: f64,
    options: DspOptions,
) -> Result<DspView, VisualizationError> {
    let target_fs = nominal_pps.round().max(1.0);
    let max_gap_ms = options.max_gap_ms.filter(|ms| ms.is_finite() && *ms >= 0.0);
    let max_gap_us = max_gap_ms.map(|ms| (ms * 1_000.0).round() as u64);

    let signal = crate::dsp::CsiSignal::from_packets(packets)?;
    let resampled = signal.resample_with(
        target_fs,
        options.interp,
        crate::dsp::ResampleOptions { max_gap_us },
    )?;

    let grid = &resampled.signal;
    let n = grid.n_samples();
    let sc = grid.n_subcarriers();
    let null_subcarriers = grid.null_bins.clone();

    // A masked column never interpolates across a real record, so its cells are
    // withheld: the renderer draws them hatched instead of as a value it would
    // have to trust.
    let time_state: Vec<u8> = resampled
        .grid_state
        .iter()
        .map(|state| state.as_u8())
        .collect();
    let masked: Vec<bool> = resampled
        .grid_state
        .iter()
        .map(|state| *state == crate::dsp::GridState::Masked)
        .collect();

    let to_matrix = |values: &ndarray::Array2<f64>| -> Vec<Vec<f32>> {
        (0..n)
            .map(|i| {
                let mut row = Vec::with_capacity(sc);
                for k in 0..sc {
                    row.push(if masked[i] {
                        f32::NAN
                    } else {
                        values[[i, k]] as f32
                    });
                }
                row
            })
            .collect()
    };

    let mut panels = vec![
        DspPanel {
            id: "amplitude".to_string(),
            title: "Amplitude — resampled".to_string(),
            unit: "a.u.".to_string(),
            kind: "sequential".to_string(),
            data: to_matrix(&grid.amplitude),
        },
        DspPanel {
            id: "phase".to_string(),
            title: "Phase — resampled".to_string(),
            unit: "rad".to_string(),
            kind: "cyclic".to_string(),
            data: to_matrix(&grid.phase),
        },
    ];

    if let Some(scheme) = options.norm {
        // Normalization runs on the resampled signal (base resampling stays the
        // default view; a normalization is an extra panel on top of it).
        let normalized = grid.normalize(scheme);
        panels.push(DspPanel {
            id: "amplitude_norm".to_string(),
            title: "Amplitude — normalized".to_string(),
            unit: if scheme == crate::dsp::NormScheme::MinMax {
                "0–1".to_string()
            } else {
                "z".to_string()
            },
            kind: "diverging".to_string(),
            data: to_matrix(&normalized.amplitude),
        });
    }

    Ok(DspView {
        interp: options.interp.as_str().to_string(),
        target_fs,
        norm: options.norm.map(norm_scheme_name),
        max_gap_ms,
        timestamps: grid.times_s().to_vec(),
        null_subcarriers,
        time_state,
        panels,
    })
}

/// Wire name of a normalization scheme (`"per_subcarrier"`, `"per_sample"`,
/// `"min_max"`).
fn norm_scheme_name(scheme: crate::dsp::NormScheme) -> String {
    match scheme {
        crate::dsp::NormScheme::PerSubcarrier => "per_subcarrier",
        crate::dsp::NormScheme::PerSample => "per_sample",
        crate::dsp::NormScheme::MinMax => "min_max",
    }
    .to_string()
}

impl VisualizationData {
    /// Load and decode a binary capture file.
    pub fn from_file(path: &Path) -> Result<Self, VisualizationError> {
        Self::from_file_with_pps(path, None)
    }

    /// Load and decode a binary capture file with an explicit nominal PPS.
    ///
    /// Without `nominal_pps` the slot grid falls back to the measured effective
    /// rate, which shrinks the grid exactly for the seconds that miss samples.
    /// Pass the configured `--pps` to keep the grid at the intended rate.
    pub fn from_file_with_pps(
        path: &Path,
        nominal_pps: Option<f64>,
    ) -> Result<Self, VisualizationError> {
        let bytes = std::fs::read(path)?;
        let source_name = path.file_name().and_then(|name| name.to_str());
        Self::from_bytes_with_pps(&bytes, source_name, nominal_pps)
    }

    /// Load and decode from raw bytes in memory.
    pub fn from_bytes(bytes: &[u8], source_name: Option<&str>) -> Result<Self, VisualizationError> {
        Self::from_bytes_with_pps(bytes, source_name, None)
    }

    /// Load and decode from raw bytes in memory with an explicit nominal PPS.
    pub fn from_bytes_with_pps(
        bytes: &[u8],
        source_name: Option<&str>,
        nominal_pps: Option<f64>,
    ) -> Result<Self, VisualizationError> {
        Self::from_bytes_with_options(
            bytes,
            source_name,
            VisualizationOptions {
                nominal_pps,
                ..VisualizationOptions::default()
            },
        )
    }

    /// Load and decode from raw bytes with explicit [`VisualizationOptions`].
    ///
    /// This is the entry point the CLI uses: it can turn the DSP view off
    /// ([`VisualizationOptions::dsp`] = `None`) or pick the interpolation,
    /// normalization and gap threshold, all of which are fixed at prepare time.
    pub fn from_bytes_with_options(
        bytes: &[u8],
        source_name: Option<&str>,
        options: VisualizationOptions,
    ) -> Result<Self, VisualizationError> {
        let packets = decode_packets(bytes)?;
        prepare_visualization_data_with_options(
            &packets,
            source_name,
            first_device_ticks(&packets),
            options,
        )
    }

    /// Attach an activity or gesture classification result to this visualization dataset.
    pub fn with_classification(
        mut self,
        predicted_class: Option<String>,
        confidence: Option<f32>,
        details: Option<String>,
    ) -> Self {
        self.classification = Some(ClassificationResult {
            predicted_class,
            confidence,
            details,
        });
        self
    }

    /// Serialize the visualization data to a JSON string.
    pub fn to_json(&self) -> Result<String, VisualizationError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Export the visualization data to a `.json` file on disk.
    pub fn export_json(&self, path: &Path) -> Result<(), VisualizationError> {
        let json_str = self.to_json()?;
        let mut file = File::create(path)?;
        file.write_all(json_str.as_bytes())?;
        Ok(())
    }

    /// Render a completely self-contained, standalone HTML document.
    ///
    /// Inlines CSS, JavaScript, and embeds `window.CSI_DATA` so the HTML file
    /// opens directly in any browser without requiring an HTTP server or network.
    pub fn render_html_bundle(&self) -> Result<String, VisualizationError> {
        let json_str = self.to_json()?;

        // Replace stylesheet link with inlined <style>
        let inlined_css = format!("<style>\n{}\n</style>", CSS_STYLES);
        let html_with_css =
            HTML_TEMPLATE.replace(r#"<link rel="stylesheet" href="style.css">"#, &inlined_css);

        // Replace script tag with inlined data payload and script
        let inlined_js = format!(
            "<script>\nwindow.CSI_DATA = {};\n</script>\n<script>\n{}\n</script>",
            json_str, JS_VISUALIZER
        );
        let bundle = html_with_css.replace(r#"<script src="visualizer.js"></script>"#, &inlined_js);

        Ok(bundle)
    }

    /// Export a standalone HTML dashboard to disk.
    pub fn export_html(&self, path: &Path) -> Result<(), VisualizationError> {
        let html = self.render_html_bundle()?;
        let mut file = File::create(path)?;
        file.write_all(html.as_bytes())?;
        Ok(())
    }
}

/// Start a lightweight, embedded asynchronous HTTP web server serving the dashboard.
///
/// Endpoints:
/// * `GET /` -> HTML dashboard
/// * `GET /api/data` -> JSON visualization data
/// * `GET /style.css` -> CSS styles
/// * `GET /visualizer.js` -> JavaScript visualizer
pub async fn serve_dashboard(
    data: VisualizationData,
    bind_addr: &str,
) -> Result<(), VisualizationError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|e| VisualizationError::Server(format!("Failed to bind to {bind_addr}: {e}")))?;

    let json_bytes = data.to_json()?.into_bytes();

    println!("Visualization web server listening on http://{bind_addr}");
    println!("Press Ctrl+C to terminate the server.");

    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|e| VisualizationError::Server(format!("Accept error: {e}")))?;

        let json_data = json_bytes.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let n = match socket.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let req_str = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_str.lines().next().unwrap_or("");
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");

            let (status, content_type, body): (&str, &str, Vec<u8>) = match path {
                "/" | "/index.html" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    HTML_TEMPLATE.as_bytes().to_vec(),
                ),
                "/style.css" => (
                    "200 OK",
                    "text/css; charset=utf-8",
                    CSS_STYLES.as_bytes().to_vec(),
                ),
                "/visualizer.js" => (
                    "200 OK",
                    "application/javascript; charset=utf-8",
                    JS_VISUALIZER.as_bytes().to_vec(),
                ),
                "/api/data" => ("200 OK", "application/json", json_data),
                _ => ("404 Not Found", "text/plain", b"Not Found".to_vec()),
            };

            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
                body.len()
            );

            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.flush().await;
        });
    }
}

/// Start a live dashboard server that serves the current snapshot on every
/// `/api/data` request.
///
/// Unlike [`serve_dashboard`], the payload is read from `state` for every
/// request, so a producer can update it (for example, every 500 ms) while the
/// browser polls. The page shell, CSS, and JavaScript are the same assets used
/// by the static dashboard.
///
/// A snapshot is either [`LiveDashboardSnapshot::Waiting`] (draw a waiting
/// message) or [`LiveDashboardSnapshot::Ready`] (draw the heatmap).
pub async fn serve_dashboard_live(
    state: Arc<RwLock<LiveDashboardSnapshot>>,
    bind_addr: &str,
) -> Result<(), VisualizationError> {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|e| VisualizationError::Server(format!("Failed to bind to {bind_addr}: {e}")))?;

    serve_dashboard_live_on(listener, state).await
}

/// Serve a live dashboard on an already-bound listener.
///
/// Binding the listener before calling this function lets a caller fail fast
/// and report the actual address (useful with port `0` in tests).
pub async fn serve_dashboard_live_on(
    listener: tokio::net::TcpListener,
    state: Arc<RwLock<LiveDashboardSnapshot>>,
) -> Result<(), VisualizationError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = listener
        .local_addr()
        .map_err(|e| VisualizationError::Server(format!("Cannot read listener address: {e}")))?;

    println!("Live visualization server listening on http://{addr}");
    println!("Open the URL above. Press Ctrl+C to stop streaming.");

    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|e| VisualizationError::Server(format!("Accept error: {e}")))?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let n = match socket.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let req_str = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_str.lines().next().unwrap_or("");
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");

            let (status, content_type, body): (&str, &str, Vec<u8>) = match path {
                "/" | "/index.html" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    HTML_TEMPLATE.as_bytes().to_vec(),
                ),
                "/style.css" => (
                    "200 OK",
                    "text/css; charset=utf-8",
                    CSS_STYLES.as_bytes().to_vec(),
                ),
                "/visualizer.js" => (
                    "200 OK",
                    "application/javascript; charset=utf-8",
                    JS_VISUALIZER.as_bytes().to_vec(),
                ),
                "/api/data" => {
                    let serialized = state
                        .read()
                        .ok()
                        .and_then(|snapshot| serde_json::to_vec(&*snapshot).ok());

                    match serialized {
                        Some(body) => ("200 OK", "application/json; charset=utf-8", body),
                        None => (
                            "500 Internal Server Error",
                            "application/json; charset=utf-8",
                            br#"{"status":"error","message":"snapshot unavailable"}"#.to_vec(),
                        ),
                    }
                }
                _ => ("404 Not Found", "text/plain", b"Not Found".to_vec()),
            };

            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
                body.len()
            );

            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.flush().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- D14: the relabel touches attribution only -------------------------

    #[test]
    fn generator_missed_is_relabelled_in_internal_mode() {
        use crate::collection::TrafficGenerator;

        // External, or unknown, keeps today's attribution.
        assert_eq!(
            layer_for(DiagnosticCause::GeneratorMissed, None),
            DiagnosticLayer::Generator
        );
        assert_eq!(
            layer_for(
                DiagnosticCause::GeneratorMissed,
                Some(TrafficGenerator::External)
            ),
            DiagnosticLayer::Generator
        );

        // Internal: the device owns the generator, so the blame moves to it.
        assert_eq!(
            layer_for(
                DiagnosticCause::GeneratorMissed,
                Some(TrafficGenerator::Internal)
            ),
            DiagnosticLayer::RxDevice
        );

        // Every other cause is mode-blind: the counters it reads are recorded
        // on the device regardless of who produced the traffic.
        for cause in [
            DiagnosticCause::DeviceDrop,
            DiagnosticCause::DesktopBackpressure,
            DiagnosticCause::AirLoss,
            DiagnosticCause::Ambiguous,
        ] {
            assert_eq!(layer_for(cause, None), cause.layer());
            assert_eq!(
                layer_for(cause, Some(TrafficGenerator::Internal)),
                cause.layer(),
                "{cause:?} must not be relabelled by the traffic mode"
            );
        }
    }

    #[test]
    fn traffic_mode_reaches_the_metadata_and_leaves_the_grid_alone() {
        use crate::collection::{NominalPpsSource, TrafficGenerator};

        let packets = capture_packets();
        let origin = packets[0].csi.device_ticks_us;

        let with_mode = |options: VisualizationOptions| {
            prepare_visualization_data_with_options(
                &packets,
                Some("capture_v8_internal.bin"),
                origin,
                options,
            )
            .expect("capture should prepare")
        };

        let internal = with_mode(VisualizationOptions {
            nominal_pps: Some(100.0),
            nominal_pps_source: Some(NominalPpsSource::Device),
            traffic_mode: Some(TrafficGenerator::Internal),
            ..VisualizationOptions::default()
        });
        assert_eq!(
            internal.metadata.traffic_mode,
            Some(TrafficGenerator::Internal)
        );
        assert_eq!(
            internal.metadata.nominal_pps_source,
            Some(NominalPpsSource::Device)
        );
        assert_eq!(internal.metadata.nominal_pps, Some(100.0));

        // The mode is display metadata: the slot grid is driven by the rate
        // alone, so an identical rate gives an identical grid.
        let external = with_mode(VisualizationOptions {
            nominal_pps: Some(100.0),
            nominal_pps_source: Some(NominalPpsSource::Argument),
            traffic_mode: Some(TrafficGenerator::External),
            ..VisualizationOptions::default()
        });
        assert_eq!(
            internal
                .diagnostics
                .as_ref()
                .map(|trace| trace.summary.clone()),
            external
                .diagnostics
                .as_ref()
                .map(|trace| trace.summary.clone()),
            "the diagnostic evidence must be identical between modes"
        );
    }

    fn capture_packets() -> Vec<CsiPacket> {
        let path = crate::fixtures::capture_v8();
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("{} should exist: {error}", path.display()));
        decode_packets(&bytes).expect("capture should decode")
    }

    #[test]
    fn origin_keeps_absolute_window_timestamps() {
        let packets = capture_packets();
        let origin = packets[0].csi.device_ticks_us;
        let window = &packets[100..];
        let expected = (window[0].csi.device_ticks_us - origin) as f64 / 1_000_000.0;

        let data =
            prepare_visualization_data_with_origin(window, Some("window"), origin, Some(20.0))
                .expect("window should prepare");

        assert_eq!(data.metadata.start_time_us, origin);
        assert_eq!(data.metadata.nominal_pps, Some(20.0));
        assert_eq!(data.metadata.total_frames, window.len());
        assert!((data.timestamps[0] - expected).abs() < 1e-12);
        // The window starts after the origin, so the offset must be applied
        // rather than reset to zero. One record past 100 at ~100 PPS is ~1 s.
        assert!(
            data.timestamps[0] > 0.5,
            "window offset not applied: {}",
            data.timestamps[0]
        );
    }

    #[test]
    fn nominal_pps_is_absent_without_a_pps_argument() {
        let packets = capture_packets();
        let data = prepare_visualization_data(&packets, None).expect("capture should prepare");

        // No `--pps` was requested: the metadata reports that honestly so the
        // dashboard can show a default next to the measured rate, while the
        // slot grid still needs a concrete rate.
        assert_eq!(data.metadata.nominal_pps, None);
        assert!(data.metadata.effective_pps > 0.0);
        let Some(trace) = data.diagnostics.as_ref() else {
            panic!("the diagnostic trace is attached");
        };
        assert_eq!(
            trace.slots_per_second,
            data.metadata.effective_pps.round().max(1.0) as usize
        );
    }

    #[tokio::test]
    async fn live_server_reports_waiting_then_ready() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral listener");
        let addr = listener.local_addr().expect("listener address");
        let state = Arc::new(RwLock::new(LiveDashboardSnapshot::Waiting {
            elapsed_seconds: 0.5,
            warmup_seconds: 3.0,
        }));
        let server = tokio::spawn(serve_dashboard_live_on(listener, Arc::clone(&state)));

        let url = format!("http://{addr}/api/data");
        let client = reqwest::Client::new();

        let waiting: serde_json::Value = client
            .get(&url)
            .send()
            .await
            .expect("waiting response")
            .json()
            .await
            .expect("waiting json");
        assert_eq!(waiting["status"], "waiting");
        assert_eq!(waiting["warmup_seconds"].as_f64(), Some(3.0));

        let packets = capture_packets();
        let data = prepare_visualization_data(&packets[..10], Some("test")).expect("small window");
        *state.write().expect("state lock") = LiveDashboardSnapshot::Ready {
            data: Box::new(data),
        };

        let ready: serde_json::Value = client
            .get(&url)
            .send()
            .await
            .expect("ready response")
            .json()
            .await
            .expect("ready json");
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["data"]["metadata"]["total_frames"].as_u64(), Some(10));

        server.abort();
        let _ = server.await;
    }
}
