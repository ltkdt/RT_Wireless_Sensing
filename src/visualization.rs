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

use crate::decoder::{CsiPacket, DecodeError, FrameStreamDecoder};

/// Embedded dashboard HTML template.
pub const HTML_TEMPLATE: &str = include_str!("../web/index.html");

/// Embedded dashboard CSS stylesheet.
pub const CSS_STYLES: &str = include_str!("../web/style.css");

/// Embedded dashboard JavaScript visualizer.
pub const JS_VISUALIZER: &str = include_str!("../web/visualizer.js");

/// Standard subcarriers per record for HT20.
pub const STANDARD_SUBCARRIERS: usize = 64;

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
    /// Nominal packet rate used to divide a one-second interval into occupancy
    /// slots. For live streaming this is the configured `--pps`; for a decoded
    /// file it defaults to [`VisualizationMetadata::effective_pps`].
    #[serde(default)]
    pub nominal_pps: f64,
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

/// Prepare [`VisualizationData`] from a slice of decoded [`CsiPacket`].
///
/// Timestamps start at `0.0` at the first packet's `device_ticks_us`.
pub fn prepare_visualization_data(
    packets: &[CsiPacket],
    source_name: Option<&str>,
) -> Result<VisualizationData, VisualizationError> {
    let origin_ticks = packets
        .first()
        .map_or(0, |packet| packet.csi.device_ticks_us);
    prepare_visualization_data_with_origin(packets, source_name, origin_ticks, None)
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
    }

    let total_frames = packets.len();
    let duration_seconds = timestamps.last().copied().unwrap_or(0.0);
    let effective_pps = if total_frames > 1 && duration_seconds > 0.0 {
        (total_frames - 1) as f64 / duration_seconds
    } else {
        0.0
    };
    let nominal_pps = nominal_pps
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(effective_pps);

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
        channel,
        device_id,
    };

    Ok(VisualizationData {
        metadata,
        timestamps,
        amplitudes,
        classification: None,
    })
}

impl VisualizationData {
    /// Load and decode a binary capture file.
    pub fn from_file(path: &Path) -> Result<Self, VisualizationError> {
        let bytes = std::fs::read(path)?;
        let packets = decode_packets(&bytes)?;
        let source_name = path.file_name().and_then(|n| n.to_str());
        prepare_visualization_data(&packets, source_name)
    }

    /// Load and decode from raw bytes in memory.
    pub fn from_bytes(bytes: &[u8], source_name: Option<&str>) -> Result<Self, VisualizationError> {
        let packets = decode_packets(bytes)?;
        prepare_visualization_data(&packets, source_name)
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
    use std::path::PathBuf;

    fn capture_packets() -> Vec<CsiPacket> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capture_v8.bin");
        let bytes = std::fs::read(path).expect("capture_v8.bin should exist");
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
        assert_eq!(data.metadata.nominal_pps, 20.0);
        assert_eq!(data.metadata.total_frames, window.len());
        assert!((data.timestamps[0] - expected).abs() < 1e-12);
        assert!(data.timestamps[0] > 4.0);
    }

    #[test]
    fn nominal_pps_falls_back_to_the_effective_rate() {
        let packets = capture_packets();
        let data = prepare_visualization_data(&packets, None).expect("capture should prepare");

        assert!((data.metadata.nominal_pps - data.metadata.effective_pps).abs() < 1e-12);
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
