//! CSI stream collection from an ESPectre device.
//!
//! This module owns the network side of the pipeline: it reads the device's
//! traffic-generator state, optionally feeds the device with UDP markers at the
//! requested packet rate, and writes the raw CSI response body to a binary
//! file. It deliberately does **not** decode the bytes; use
//! [`crate::decoder`] for that and [`crate::saving`] to produce numeric arrays.
//!
//! # The capture path is read-only
//!
//! Collection never changes the device's settings. It reads `GET /sensing` once
//! through [`crate::device`] to learn the current traffic mode and target rate,
//! and refuses to run when the device is in the wrong mode for the requested
//! traffic source ([`crate::device::require_mode`]). Changing the mode is an
//! explicit operator action, offered by the `device` binary: the setting is
//! NVS-persistent, so a collector that patched it would leave the device in a
//! mode that outlives the run.
//!
//! The collected file is exactly the byte stream returned by
//! `GET /espectre/v1/csi`, so every frame starts with the 60-byte
//! [`crate::decoder::HttpFrameHeader`] followed by a 64-byte CSI record header
//! and its I/Q payload.
//!
//! # Example
//!
//! ```no_run
//! use rt_wireless_sensing::collection::{collect, CollectionConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = CollectionConfig {
//!     esp32_ip: "192.168.1.50".to_string(),
//!     output: "capture.bin".into(),
//!     ..CollectionConfig::default()
//! };
//! let summary = collect(&config).await?;
//! println!("wrote {} bytes", summary.bytes_written);
//! # Ok(())
//! # }
//! ```

use std::{
    error::Error,
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncWriteExt,
    net::UdpSocket,
    time::{interval, sleep},
};

use crate::csi_header::CSI_HEADER_LEN;
use crate::decoder::{HTTP_FRAME_PREFIX_LEN, HttpFrameHeader};
use crate::device::{self, DeviceEndpoint, DeviceStatus};

pub use crate::device::TrafficGenerator;

/// Default ESP32 address used by the project.
pub const DEFAULT_ESP32_IP: &str = "10.178.22.9";

/// Default ESPectre HTTP API port.
pub const DEFAULT_API_PORT: u16 = 62_587;

/// Default UDP port the ESPectre firmware listens on for traffic markers.
pub const DEFAULT_MARKER_PORT: u16 = 5_555;

/// Default marker rate in packets per second.
pub const DEFAULT_PPS: u32 = 20;

/// Default HTTP `Origin` header expected by ESPectre.
pub const DEFAULT_ORIGIN: &str = "https://test.espectre.dev";

/// Default capture file written in the current working directory.
pub const DEFAULT_OUTPUT: &str = "capture_v8.bin";

/// Four-byte marker the ESPectre firmware accepts as a traffic stimulus.
///
/// The bytes are the UTF-8 encoding of the "👻" emoji, as used by ESPectre's
/// own tooling.
pub const TRAFFIC_MARKER: [u8; 4] = [0xF0, 0x9F, 0x91, 0xBB];

/// Settings for one collection run.
///
/// [`CollectionConfig::default`] reproduces the project's original behavior:
/// the default ESP32 address, 20 PPS, and `capture_v8.bin` as the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionConfig {
    /// ESP32 IPv4 address used for both the HTTP API and UDP markers.
    pub esp32_ip: String,
    /// TCP port of the ESPectre HTTP API.
    pub api_port: u16,
    /// UDP port that receives the traffic markers.
    ///
    /// Only used when [`Self::markers_enabled`] is true.
    pub marker_port: u16,
    /// Marker rate; must be greater than zero in external mode.
    ///
    /// In internal mode this is the **fallback** nominal rate: the device's own
    /// `csi_target_pps` outranks it, and nothing is sent at this rate.
    pub pps: u32,
    /// Who produces the traffic being captured.
    ///
    /// [`TrafficGenerator::External`] (the default) means this process sends
    /// the UDP markers; [`TrafficGenerator::Internal`] means the device
    /// generates its own traffic and no markers are sent.
    pub traffic_generator: TrafficGenerator,
    /// `Origin` header sent with every HTTP request.
    pub origin: String,
    /// Destination of the raw CSI byte stream. Overwritten if it exists.
    pub output: PathBuf,
    /// Whether [`Self::output`] is still the default path.
    ///
    /// [`Self::resolved_output`] uses this to decide whether the capture name
    /// may gain the `_internal` suffix: an explicit `--output` is honoured
    /// verbatim. Set to `false` by every CLI that accepts `--output` and by
    /// [`Self::with_output`].
    pub output_is_default: bool,
    /// Delay between enabling sensing and opening the CSI stream.
    ///
    /// The original streamer waited 200 ms so the ESP32 could start sampling.
    pub startup_delay: Duration,
    /// Stop recording after this long.
    ///
    /// `None` (the default) records until the device closes the stream.
    pub max_duration: Option<Duration>,
    /// Print a progress line to stderr for every received chunk.
    pub verbose: bool,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            esp32_ip: DEFAULT_ESP32_IP.to_string(),
            api_port: DEFAULT_API_PORT,
            marker_port: DEFAULT_MARKER_PORT,
            pps: DEFAULT_PPS,
            traffic_generator: TrafficGenerator::External,
            origin: DEFAULT_ORIGIN.to_string(),
            output: PathBuf::from(DEFAULT_OUTPUT),
            output_is_default: true,
            startup_delay: Duration::from_millis(200),
            max_duration: None,
            verbose: true,
        }
    }
}

impl CollectionConfig {
    /// Build a configuration that writes to `output` and uses the other
    /// defaults.
    ///
    /// The explicit path clears [`Self::output_is_default`], so
    /// [`Self::resolved_output`] never renames it.
    pub fn with_output(output: impl Into<PathBuf>) -> Self {
        Self {
            output: output.into(),
            output_is_default: false,
            ..Self::default()
        }
    }

    /// Whether this run sends UDP markers itself.
    ///
    /// Marker mode only: [`Self::marker_port`], [`Self::marker_period`] and
    /// [`Self::marker_target`] are meaningless when the device generates its
    /// own traffic.
    pub fn markers_enabled(&self) -> bool {
        self.traffic_generator == TrafficGenerator::External
    }

    /// The nominal rate implied by [`Self::pps`], if one was configured.
    ///
    /// This is the single place a CLI `--pps` value becomes a *nominal* rate,
    /// shared by both capture paths. It says nothing about who produces the
    /// traffic: in internal mode the device's `csi_target_pps` outranks it and
    /// this value is only a fallback.
    pub fn nominal_pps(&self) -> Option<f64> {
        (self.pps > 0).then_some(f64::from(self.pps))
    }

    /// ESPectre API base URL derived from [`Self::esp32_ip`] and
    /// [`Self::api_port`].
    pub fn base_url(&self) -> String {
        format!("http://{}:{}/espectre/v1", self.esp32_ip, self.api_port)
    }

    /// The device endpoint these settings describe.
    pub fn device_endpoint(&self) -> DeviceEndpoint {
        DeviceEndpoint::new(self.esp32_ip.clone(), self.api_port, self.origin.clone())
    }

    /// Marker period implied by [`Self::pps`], clamped to at least 1 ms.
    ///
    /// Marker mode only.
    pub fn marker_period(&self) -> Duration {
        Duration::from_millis((1_000 / u64::from(self.pps.max(1))).max(1))
    }

    /// Address that receives the UDP markers, which is [`Self::esp32_ip`] and [`Self::marker_port`].
    ///
    /// Marker mode only.
    pub fn marker_target(&self) -> Result<SocketAddr, CollectionError> {
        let endpoint = format!("{}:{}", self.esp32_ip, self.marker_port);
        endpoint
            .parse()
            .map_err(|source| CollectionError::InvalidEndpoint {
                endpoint: endpoint.clone(),
                source,
            })
    }

    /// The path this run actually writes.
    ///
    /// An internal run that keeps the default output gains an `_internal`
    /// suffix (`capture_v8.bin` → `capture_v8_internal.bin`) so two
    /// consecutive runs, one per mode, cannot overwrite each other. An
    /// explicit `--output` is never renamed, because the operator already chose
    /// a name. Use [`traffic_mode_from_filename`] to read the hint back.
    pub fn resolved_output(&self) -> PathBuf {
        if !self.output_is_default || self.traffic_generator != TrafficGenerator::Internal {
            return self.output.clone();
        }

        let Some(stem) = self.output.file_stem().and_then(|stem| stem.to_str()) else {
            return self.output.clone();
        };
        let renamed = format!("{stem}_internal");
        match self
            .output
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some(extension) => self.output.with_file_name(format!("{renamed}.{extension}")),
            None => self.output.with_file_name(renamed),
        }
    }

    /// Reject settings that would fail later in the run.
    ///
    /// Marker-only settings are only checked when this run sends markers
    /// ([`Self::markers_enabled`]): an internal run legitimately has no marker
    /// rate and no marker target. The struct itself stays total, so `pps: 0`
    /// and a hostname-shaped `esp32_ip` are valid values to *hold*, just not
    /// valid values to send markers with.
    fn validate(&self) -> Result<(), CollectionError> {
        if self.markers_enabled() {
            if self.pps == 0 {
                return Err(CollectionError::InvalidPps);
            }
            self.marker_target()?;
        }
        Ok(())
    }

    /// The nominal packet rate and where it came from.
    ///
    /// This is the one ladder both capture paths use, so the dashboard and the
    /// recorded metadata can never disagree about the nominal rate:
    ///
    /// 1. **Internal mode** — the device owns the rate: its `csi_target_pps`
    ///    wins, and the source is [`NominalPpsSource::Device`].
    /// 2. **Internal mode, device unreadable or silent** — fall back to
    ///    [`Self::nominal_pps`], source [`NominalPpsSource::Argument`].
    /// 3. **External mode** — the argument only. The device's rate is
    ///    deliberately *not* consulted: this process owns the cadence here, so
    ///    a device value would describe traffic nobody is sending.
    ///
    /// `None` means "nothing declared a rate"; consumers display the measured
    /// rate instead of inventing one.
    pub fn resolve_nominal(
        &self,
        status: Option<&DeviceStatus>,
    ) -> (Option<f64>, Option<NominalPpsSource>) {
        match self.traffic_generator {
            TrafficGenerator::Internal => match status.and_then(|status| status.csi_target_pps) {
                Some(pps) => (Some(f64::from(pps)), Some(NominalPpsSource::Device)),
                None => match self.nominal_pps() {
                    Some(pps) => (Some(pps), Some(NominalPpsSource::Argument)),
                    None => (None, None),
                },
            },
            TrafficGenerator::External => match self.nominal_pps() {
                Some(pps) => (Some(pps), Some(NominalPpsSource::Argument)),
                None => (None, None),
            },
        }
    }
}

/// Where the nominal packet rate in a payload came from.
///
/// Serialized lowercase (`"device"` / `"argument"`) so the web UI can decide
/// whether the number describes the planned cadence or the device's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NominalPpsSource {
    /// Read from the device's `csi_target_pps`.
    Device,
    /// Taken from the command line's `--pps`.
    Argument,
}

impl NominalPpsSource {
    /// Human-readable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Argument => "argument",
        }
    }
}

impl fmt::Display for NominalPpsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Read the traffic mode back out of a capture file name.
///
/// The inverse of [`CollectionConfig::resolved_output`]: a file whose stem ends
/// with `_internal` was written by an internal run. Anything else returns
/// `None` — *unknown*, not `External` — because a name proves nothing about an
/// arbitrary file, and [`crate::visualization`] renders unknown as `—` rather
/// than guessing.
pub fn traffic_mode_from_filename(name: &str) -> Option<TrafficGenerator> {
    let stem = Path::new(name).file_stem().and_then(|stem| stem.to_str())?;
    stem.ends_with("_internal")
        .then_some(TrafficGenerator::Internal)
}

/// What a successful collection run produced.
///
/// Not `PartialEq`: [`Self::nominal_pps`] is a float, and the summary is
/// report data rather than a value to compare.
#[derive(Debug, Clone)]
pub struct CollectionSummary {
    /// File the raw stream was written to.
    pub output: PathBuf,
    /// Total number of bytes written.
    pub bytes_written: u64,
    /// Wall-clock duration of the streaming phase.
    pub duration: Duration,
    /// The `--pps` value in force.
    ///
    /// In internal mode this is the *fallback* rate: the device's own target
    /// outranks it and nothing is sent at it. Who produces the traffic is
    /// described by [`Self::traffic_mode`], not by nulling this number.
    pub configured_pps: u32,
    /// Who produced the traffic this run recorded.
    pub traffic_mode: TrafficGenerator,
    /// The `traffic_generator_mode` the device reported, verbatim.
    ///
    /// `None` when the device could not be read. Kept as the raw string rather
    /// than [`TrafficGenerator`] so `ping` and `wifi_raw` stay distinguishable
    /// in diagnostics.
    pub device_traffic_mode: Option<String>,
    /// The nominal packet rate this run was planned around.
    pub nominal_pps: Option<f64>,
    /// Where [`Self::nominal_pps`] came from.
    pub nominal_pps_source: Option<NominalPpsSource>,
    /// Raw JSON text returned by `GET /capabilities`.
    pub capabilities: String,
}

/// Failure modes of [`collect`].
#[derive(Debug)]
pub enum CollectionError {
    /// An HTTP request failed or returned a non-success status.
    Http(reqwest::Error),
    /// Reading the response body or writing the output file failed.
    Io(std::io::Error),
    /// The device address could not be parsed as a socket address.
    InvalidEndpoint {
        /// The endpoint that failed to parse.
        endpoint: String,
        /// Underlying address-parsing error.
        source: std::net::AddrParseError,
    },
    /// [`CollectionConfig::pps`] was zero.
    InvalidPps,
    /// The device's traffic mode contradicts the one this run needs.
    ///
    /// Fatal: the capture would record nothing (internal requested, device in
    /// `external`) or a second, uncounted stimulus (external requested, device
    /// generating its own traffic).
    WrongDeviceMode(Box<device::Mismatch>),
    /// Any other device failure surfaced while preparing the run.
    Device(device::DeviceError),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(f, "HTTP request failed: {error}"),
            Self::Io(error) => write!(f, "I/O error while collecting CSI: {error}"),
            Self::InvalidEndpoint { endpoint, source } => {
                write!(f, "invalid device endpoint {endpoint:?}: {source}")
            }
            Self::InvalidPps => write!(f, "PPS must be greater than zero"),
            Self::WrongDeviceMode(mismatch) => write!(f, "{mismatch}"),
            Self::Device(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CollectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::InvalidEndpoint { source, .. } => Some(source),
            Self::InvalidPps => None,
            Self::WrongDeviceMode(mismatch) => Some(mismatch),
            Self::Device(error) => Some(error),
        }
    }
}

impl From<reqwest::Error> for CollectionError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<std::io::Error> for CollectionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<device::DeviceError> for CollectionError {
    fn from(error: device::DeviceError) -> Self {
        match error {
            device::DeviceError::Mismatch(mismatch) => Self::WrongDeviceMode(mismatch),
            // `require_mode` cannot produce the other variants; keep them
            // reachable rather than panicking on an unreachable branch.
            other => Self::Device(other),
        }
    }
}

/// Aborts a spawned task when this guard is dropped.
///
/// Keeping the handle in a guard means cancellation of [`collect`] (dropping
/// its future) also stops the UDP marker task instead of detaching it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Collect the ESPectre CSI stream into `config.output`.
///
/// The function performs this sequence, and changes nothing on the device:
///
/// 1. `GET /capabilities` to confirm the endpoint is reachable.
/// 2. `GET /sensing` to learn the device's traffic mode and `csi_target_pps`.
///    This is a read: the collector never patches the device.
/// 3. Verify the mode matches [`CollectionConfig::traffic_generator`] and stop
///    with [`CollectionError::WrongDeviceMode`] if it does not.
/// 4. In external mode, spawn a UDP marker sender at [`CollectionConfig::pps`].
/// 5. Wait [`CollectionConfig::startup_delay`].
/// 6. `GET /csi` and copy every response chunk verbatim into the output file.
///
/// The marker task is stopped when the stream ends or an error occurs. The
/// returned [`CollectionSummary`] contains the device state, the resolved
/// nominal rate, and byte and duration counters.
///
/// This is the convenience form of [`collect_with_sink`] with a no-op chunk
/// callback.
///
/// # Cancellation
///
/// This future completes when the HTTP body ends (the device closes the
/// stream), when [`CollectionConfig::max_duration`] elapses, or on an
/// I/O/HTTP/device-mode error. Dropping the future aborts the HTTP request and
/// the marker task.
pub async fn collect(config: &CollectionConfig) -> Result<CollectionSummary, CollectionError> {
    collect_with_sink(config, |_| {}).await // Pass empty closure as the chunk callback.
}

/// Collect the CSI stream and call `on_chunk` for every response chunk.
///
/// This behaves exactly like [`collect`], but each chunk is also passed to the
/// callback after it has been written to [`CollectionConfig::output`]. The
/// callback can feed an incremental [`crate::decoder::FrameStreamDecoder`] so a
/// live consumer sees records while the raw `.bin` capture is still being
/// written.
///
/// The callback is synchronous and runs on the collection task, so it should
/// do only cheap work (for example, forward the chunk to a channel).
///
/// # Cancellation
///
/// Same as [`collect`]: dropping the future aborts the HTTP request, the
/// marker task, and any remaining chunk delivery.
pub async fn collect_with_sink<F>(
    config: &CollectionConfig,
    mut on_chunk: F,
) -> Result<CollectionSummary, CollectionError>
where
    F: FnMut(&[u8]),
{
    config.validate()?;

    let client = Client::new();
    let base = config.base_url();

    // 1. Check CSI capability.
    let capabilities = client
        .get(format!("{base}/capabilities"))
        .header("Origin", &config.origin)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // 2. Read the device state. Read-only: the capture path never writes, in
    //    either mode, because `traffic_generator_mode` is NVS-persistent and a
    //    patched value would outlive this run.
    let device_status = device::read(&config.device_endpoint()).await;

    // 3. Refuse to capture in the wrong mode, before any file is created.
    if let Err(error) = device::require_mode(device_status.as_ref(), config.traffic_generator) {
        return Err(error.into());
    }

    // 4. Resolve the nominal rate the same way the live dashboard does.
    let (nominal_pps, nominal_pps_source) = config.resolve_nominal(device_status.as_ref());

    // 5. Send UDP markers at the configured rate, external mode only. The
    //    guard aborts the task when it is dropped, including on cancellation.
    let marker_task = if config.markers_enabled() {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let sender = Arc::clone(&socket);
        let target = config.marker_target()?;
        let period = config.marker_period();
        Some(AbortOnDrop(tokio::spawn(async move {
            let mut timer = interval(period);
            loop {
                timer.tick().await;
                if sender.send_to(&TRAFFIC_MARKER, target).await.is_err() {
                    break;
                }
            }
        })))
    } else {
        None
    };

    // 6. Give the ESP32 time to start sampling before opening the stream.
    sleep(config.startup_delay).await;

    // 7. Stream the response body into the output file.
    let streaming = stream_to_file(config, &client, &base, &mut on_chunk).await;

    drop(marker_task);
    let (bytes_written, duration) = streaming?;

    Ok(CollectionSummary {
        output: config.resolved_output(),
        bytes_written,
        duration,
        configured_pps: config.pps,
        traffic_mode: config.traffic_generator,
        device_traffic_mode: device_status.and_then(|status| status.traffic_generator_mode),
        nominal_pps,
        nominal_pps_source,
        capabilities,
    })
}

/// Open `/csi` and copy the response body into the configured file.
///
/// After each chunk is written, `on_chunk` is called with the same bytes.
///
/// # Where a `max_duration` run ends
///
/// The output file stops at the last record whose device timestamp is still
/// **strictly before** `max_duration`, measured from the first record of the
/// session. Records past that point are neither written nor passed to
/// `on_chunk`: a capture declares its own span, so a visualiser reading the
/// file alone never has to guess where the run was supposed to stop. A 5 s run
/// therefore holds 500 records, not 501.
///
/// The cut needs the device ticks, which only a decoded frame carries, so the
/// chunks are scanned with a private [`FrameStreamDecoder`]. The scan never
/// fails the run: a malformed frame simply ends the scanning, and the trailing
/// timer keeps a broken stream from being recorded forever.
async fn stream_to_file<F>(
    config: &CollectionConfig,
    client: &Client,
    base: &str,
    on_chunk: &mut F,
) -> Result<(u64, Duration), CollectionError>
where
    F: FnMut(&[u8]), // F is a synchronous callback that runs on the collection task.
{
    let response = client
        .get(format!("{base}/csi"))
        .header("Origin", &config.origin)
        .send()
        .await?
        .error_for_status()?;

    let started = Instant::now();
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(config.resolved_output()).await?;
    let mut total = 0u64;
    let mut deadline = config.max_duration.map(|max| Box::pin(sleep(max)));
    let mut cut = config
        .max_duration
        .map(|max| DurationCut::new(max.as_micros() as u64));

    loop {
        let next = stream.next();
        let chunk = match deadline.as_mut() {
            Some(timer) => tokio::select! {
                chunk = next => chunk,
                _ = timer.as_mut() => break,
            },
            None => next.await,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk?;

        // Where inside the chunk the records stop being part of the run.
        // `accept` reports how much of `chunk` follows the bytes it already
        // held, so the write covers the new bytes only and never a frame twice.
        let keep = match cut.as_mut() {
            Some(cut) => chunk.len() - cut.accept(&chunk),
            None => chunk.len(),
        };
        let written = &chunk[chunk.len() - keep..];

        if !written.is_empty() {
            file.write_all(written).await?;
            total += written.len() as u64;
            on_chunk(written);
            if config.verbose {
                eprintln!("Received {total} bytes");
            }
        }

        // The cut is final once a record `>= max_duration` arrived, whether it
        // was inside this chunk or the chunk carried the whole tail.
        if cut.as_ref().is_some_and(DurationCut::is_reached) {
            break;
        }
    }

    // The deadline, not the stream, ends a run: whatever the last chunk held
    // back is a truncated tail, which belongs in the file exactly as received.
    let rest = cut.as_ref().map(DurationCut::flush).unwrap_or(&[]);
    if !rest.is_empty() {
        file.write_all(rest).await?;
        total += rest.len() as u64;
        on_chunk(rest);
    }
    file.flush().await?;

    Ok((total, started.elapsed()))
}

/// Finds the byte offset in the stream where a `max_duration` run ends.
///
/// The comparison is done on device ticks, not on the wall clock, so the
/// result does not depend on prompt-delivered chunks, TCP scheduling, or how
/// much the host slept between reads. The first decoded frame defines the
/// origin; a record is kept when `ticks - origin < max_ticks`.
///
/// The cut never sees a record twice, and the file it produces ends on a frame
/// boundary: a frame that straddles the limit is dropped whole rather than
/// half-written, because half a record is not a shorter record.
struct DurationCut {
    /// The limit, converted from [`CollectionConfig::max_duration`].
    max_ticks: u64,
    /// Device ticks of the first record seen, once one was decoded.
    origin_ticks: Option<u64>,
    /// Set once a record at or past the limit was found: the run ends here.
    reached: bool,
    /// Bytes held back from the previous chunk because the frame they start is
    /// not complete yet. They are settled by the next [`Self::accept`] or
    /// returned by [`Self::flush`].
    carry: Vec<u8>,
}

impl DurationCut {
    fn new(max_ticks: u64) -> Self {
        Self {
            max_ticks,
            origin_ticks: None,
            reached: false,
            carry: Vec::new(),
        }
    }

    /// Whether the cut has already been found.
    fn is_reached(&self) -> bool {
        self.reached
    }

    /// Return the number of leading bytes of `chunk` that belong to the run.
    ///
    /// The caller writes `chunk[..keep]`; the bytes after that index are not
    /// written now, because they are not settled yet. A frame that the chunk
    /// does not contain entirely sits at that boundary: it is moved into the
    /// carry buffer so the next call can judge it in one piece.
    ///
    /// The count always sits on a frame boundary. When a complete frame is at
    /// or past the limit, `reached` is set and the count stops just before it,
    /// so a frame straddling the limit is dropped whole.
    fn accept(&mut self, chunk: &[u8]) -> usize {
        // Frame boundaries are positions inside `frame_stream`, which starts
        // with the carry, so a position there is a position in the run.
        let mut frame_stream = std::mem::take(&mut self.carry);
        let held = frame_stream.len();
        frame_stream.extend_from_slice(chunk);

        let mut settled = 0;
        while settled + HTTP_FRAME_PREFIX_LEN <= frame_stream.len() {
            // A malformed prefix cannot be sized, so it also cannot be placed
            // on the timeline: stop settling and let the caller end the run.
            let Ok(prefix) =
                HttpFrameHeader::parse(&frame_stream[settled..][..HTTP_FRAME_PREFIX_LEN], 0)
            else {
                break;
            };
            let frame_len = prefix.frame_len();
            if frame_len > frame_stream.len() - settled {
                // The frame runs past this chunk; the next one settles it.
                break;
            }

            // Only read the device tick: a frame this cut has to drop is often
            // the first one whose counters or payload are inconsistent, and
            // accepting only fully valid records would keep frames past the
            // limit instead of dropping them.
            let Some(ticks) = read_device_ticks(&frame_stream[settled..][..frame_len]) else {
                break;
            };

            let origin = *self.origin_ticks.get_or_insert(ticks);
            if ticks.saturating_sub(origin) >= self.max_ticks {
                // This record and everything after it are past the run.
                self.reached = true;
                break;
            }
            settled += frame_len;
        }

        // Whatever is past `settled` is either the start of a frame the next
        // chunk completes, or the tail past the limit that `flush` withholds.
        self.carry = frame_stream.split_off(settled);
        // The carry may start before this chunk did, and those bytes were
        // already written by an earlier call, so only the part that came from
        // `chunk` counts as settled now.
        settled.saturating_sub(held)
    }

    /// Bytes still held back when the stream ends.
    ///
    /// They never formed a complete frame, so they belong to the run unless the
    /// cut fired first; when the deadline ended the run, a truncated tail is
    /// exactly what the deadline produces and it is written as received.
    fn flush(&self) -> &[u8] {
        if self.reached {
            &[]
        } else {
            &self.carry
        }
    }
}

/// Device ticks of the record in `frame`, or `None` when the record cannot be
/// located.
///
/// The cut only needs `device_ticks_us`, so it reads that single field instead
/// of decoding the record: the frame at the limit is often the first one whose
/// counters or payload are inconsistent, and those records still have to be
/// recognised (to be dropped) rather than treated as the end of the timeline.
fn read_device_ticks(frame: &[u8]) -> Option<u64> {
    let record = frame.get(HTTP_FRAME_PREFIX_LEN..)?;
    if record.len() < usize::from(CSI_HEADER_LEN) {
        return None;
    }
    // Layout: <HBBBBIHHQQIQBbbQIIBBB>; `device_ticks_us` is the u64 at byte 20.
    const DEVICE_TICKS_OFFSET: usize = 20;
    let bytes = record.get(DEVICE_TICKS_OFFSET..DEVICE_TICKS_OFFSET + 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

impl CollectionSummary {
    /// The `device` line, for the operator-facing summary.
    ///
    /// States the mode the device reported, and why it is `unknown` when the
    /// read failed — the one case where the run continues without having
    /// verified the pairing.
    pub fn device_line(&self) -> String {
        match (&self.device_traffic_mode, self.traffic_mode) {
            (Some(mode), TrafficGenerator::Internal) => {
                format!(
                    "traffic_generator_mode={mode} (internal, the device generates its own traffic)"
                )
            }
            (Some(mode), TrafficGenerator::External) => {
                format!("traffic_generator_mode={mode} (external, markers are sent by this host)")
            }
            (None, _) => "unknown (GET /sensing failed; mode unverified)".to_string(),
        }
    }

    /// The `rate` line: the resolved nominal rate and its provenance.
    pub fn rate_line(&self) -> String {
        match (self.nominal_pps, self.nominal_pps_source) {
            (Some(pps), Some(source)) => format!("{pps} PPS (nominal, from the {source})"),
            (Some(pps), None) => format!("{pps} PPS (nominal)"),
            (None, _) => format!(
                "{} PPS (marker rate; no nominal rate declared)",
                self.configured_pps
            ),
        }
    }
}



#[cfg(test)]
mod duration_cut_tests {
    use super::DurationCut;
    use crate::csi_header::{CSI_HEADER_LEN, RAW_CSI_RECORD_MAGIC, RAW_CSI_RECORD_VERSION_V8};
    use crate::decoder::{HTTP_FRAME_PREFIX_LEN, HTTP_FRAME_PREFIX_LEN_U16};

    /// How many frames the synthetic streams carry.
    const FRAMES: u64 = 8;

    /// Build one synthetic frame with `payload` bytes of CSI.
    ///
    /// Only `device_ticks_us` matters to the cut and the frame is never decoded
    /// here, so the prefix only has to be long enough to be sized.
    fn frame(ticks_us: u64, sequence: u64, payload: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; HTTP_FRAME_PREFIX_LEN + CSI_HEADER_LEN as usize + payload];
        let record_len = u16::from(CSI_HEADER_LEN) + payload as u16;

        bytes[6..8].copy_from_slice(&HTTP_FRAME_PREFIX_LEN_U16.to_le_bytes());
        bytes[24..32].copy_from_slice(&sequence.to_le_bytes());
        bytes[32..34].copy_from_slice(&record_len.to_le_bytes());

        let record = HTTP_FRAME_PREFIX_LEN;
        bytes[record..record + 2].copy_from_slice(&RAW_CSI_RECORD_MAGIC.to_le_bytes());
        bytes[record + 2] = RAW_CSI_RECORD_VERSION_V8;
        bytes[record + 3] = CSI_HEADER_LEN;
        // `device_ticks_us` is the u64 at byte 20 of the record header.
        bytes[record + 20..record + 28].copy_from_slice(&ticks_us.to_le_bytes());
        bytes
    }

    /// A stream of `FRAMES` frames, `step_us` apart, starting at tick 0.
    fn stream(step_us: u64) -> Vec<u8> {
        (0..FRAMES)
            .map(|index| frame(index * step_us, index + 1, 64))
            .flatten()
            .collect()
    }

    /// Feed `stream` to the cut in chunks of `chunk_len` bytes and return the
    /// bytes a capture would contain, mirroring `stream_to_file`.
    /// Feed `stream` to the cut in chunks of `chunk_len` bytes and return the
    /// bytes a capture would contain, mirroring `stream_to_file`.
    fn run(cut: &mut DurationCut, stream: &[u8], chunk_len: usize) -> Vec<u8> {
        let mut kept = Vec::new();
        for chunk in stream.chunks(chunk_len) {
            // `accept` returns how many leading bytes of `chunk` are settled and
            // inside the run, so the write covers those and never the carry twice.
            kept.extend_from_slice(&chunk[..cut.accept(chunk)]);
            if cut.is_reached() {
                break;
            }
        }
        kept.extend_from_slice(cut.flush());
        kept
    }
    #[test]
    fn cut_keeps_records_strictly_before_the_limit() {
        // 100 ms apart with a 350 ms limit: the frames at 0, 100, 200 and
        // 300 ms are inside, the one at 400 ms is the first past the limit.
        let mut cut = DurationCut::new(350_000);
        let data = stream(100_000);

        let frame_len = data.len() / FRAMES as usize;
        let kept = run(&mut cut, &data, frame_len * 2);

        assert_eq!(kept.len(), 4 * frame_len);
        assert_eq!(kept.len() % frame_len, 0, "the cut lands on a boundary");
        assert!(cut.is_reached());
    }

    #[test]
    fn cut_handles_the_limit_inside_one_chunk() {
        // The same run, but the socket hands the whole stream over at once, so
        // the cut has to stop in the middle of what it was given.
        let mut cut = DurationCut::new(350_000);
        let data = stream(100_000);

        let frame_len = data.len() / FRAMES as usize;
        let kept = run(&mut cut, &data, data.len());

        assert_eq!(kept.len(), 4 * frame_len);
        assert!(cut.is_reached());
    }

    #[test]
    fn cut_without_a_reachable_limit_keeps_everything() {
        // A limit far beyond the stream must never truncate it.
        let mut cut = DurationCut::new(u64::MAX);
        let data = stream(10_000);

        let kept = run(&mut cut, &data, 4_096);

        assert_eq!(kept, data);
        assert!(!cut.is_reached());
    }

    #[test]
    fn a_partial_tail_is_kept_only_when_the_deadline_ends_the_run() {
        // A chunk may end in the middle of a frame. If the cut never fired, the
        // deadline ended the run and the truncated tail is part of the capture.
        let mut cut = DurationCut::new(1_000_000);
        let data = stream(100_000);
        let frame_len = data.len() / FRAMES as usize;

        let partial = &data[..2 * frame_len + 10];
        let kept = run(&mut cut, partial, partial.len());

        assert_eq!(kept, partial);
        assert!(!cut.is_reached());
    }

    #[test]
    fn a_frame_split_by_a_chunk_boundary_may_be_dropped_whole() {
        // Same limit, but the socket splits the last frame inside the run:
        // its 10-byte start arrives with the frames before it and the rest
        // with the next chunk. Neither piece may reach the file, because half
        // a record is not a shorter record.
        let mut cut = DurationCut::new(350_000);
        let data = stream(100_000);
        let frame_len = data.len() / FRAMES as usize;
        // 4 whole frames plus the first 10 bytes of the fifth.
        let chunks = 4 * frame_len + 10;

        let kept = run(&mut cut, &data[..5 * frame_len - 10], chunks);

        assert_eq!(kept.len(), 4 * frame_len);
        assert_eq!(kept.len() % frame_len, 0, "the cut lands on a boundary");
        assert!(cut.is_reached());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_the_project_defaults() {
        let config = CollectionConfig::default();
        assert_eq!(config.esp32_ip, DEFAULT_ESP32_IP);
        assert_eq!(config.api_port, DEFAULT_API_PORT);
        assert_eq!(config.marker_port, DEFAULT_MARKER_PORT);
        assert_eq!(config.pps, DEFAULT_PPS);
        assert_eq!(config.output, PathBuf::from(DEFAULT_OUTPUT));
    }

    #[test]
    fn base_url_contains_the_api_prefix() {
        let config = CollectionConfig::default();
        assert_eq!(config.base_url(), "http://10.178.22.9:62587/espectre/v1");
    }

    #[test]
    fn marker_period_uses_pps_and_never_rounds_to_zero() {
        let mut config = CollectionConfig::default();
        assert_eq!(config.marker_period(), Duration::from_millis(50));

        config.pps = 3;
        assert_eq!(config.marker_period(), Duration::from_millis(333));

        config.pps = 10_000;
        assert_eq!(config.marker_period(), Duration::from_millis(1));
    }

    #[test]
    fn marker_target_combines_ip_and_port() {
        let config = CollectionConfig::default();
        assert_eq!(
            config.marker_target().unwrap(),
            "10.178.22.9:5555".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn zero_pps_is_rejected() {
        let config = CollectionConfig {
            pps: 0,
            ..CollectionConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(CollectionError::InvalidPps)
        ));
    }

    #[test]
    fn max_duration_defaults_to_unlimited() {
        assert_eq!(CollectionConfig::default().max_duration, None);

        let config = CollectionConfig {
            max_duration: Some(Duration::from_secs(20)),
            ..CollectionConfig::default()
        };
        assert_eq!(config.max_duration, Some(Duration::from_secs(20)));
        assert!(config.validate().is_ok());
    }

    // --- D5: the mode is an enum with an external default -------------------

    #[test]
    fn traffic_generator_defaults_to_external() {
        let config = CollectionConfig::default();
        assert_eq!(config.traffic_generator, TrafficGenerator::External);
        assert!(config.markers_enabled());
        assert!(config.output_is_default);
    }

    // --- §1.3: marker-only settings are only checked in marker mode ---------

    #[test]
    fn internal_mode_validates_without_pps_or_target() {
        let internal = CollectionConfig {
            pps: 0,
            esp32_ip: "not-an-ip".to_string(),
            traffic_generator: TrafficGenerator::Internal,
            ..CollectionConfig::default()
        };
        assert!(
            internal.validate().is_ok(),
            "an internal run needs no marker rate and no marker target"
        );
        assert!(!internal.markers_enabled());

        let external = CollectionConfig {
            traffic_generator: TrafficGenerator::External,
            ..internal.clone()
        };
        assert!(matches!(
            external.validate(),
            Err(CollectionError::InvalidPps)
        ));
    }

    #[test]
    fn nominal_pps_is_the_argument_or_nothing() {
        for mode in [TrafficGenerator::External, TrafficGenerator::Internal] {
            let with = CollectionConfig {
                pps: 100,
                traffic_generator: mode,
                ..CollectionConfig::default()
            };
            assert_eq!(with.nominal_pps(), Some(100.0));

            let without = CollectionConfig {
                pps: 0,
                traffic_generator: mode,
                ..CollectionConfig::default()
            };
            assert_eq!(without.nominal_pps(), None);
        }
    }

    // --- D11: the nominal ladder, in both modes ------------------------------

    fn status(mode: &str, pps: Option<u32>) -> DeviceStatus {
        DeviceStatus {
            traffic_generator_mode: Some(mode.to_string()),
            csi_target_pps: pps,
        }
    }

    #[test]
    fn internal_mode_prefers_the_device_rate() {
        let config = CollectionConfig {
            pps: 50,
            traffic_generator: TrafficGenerator::Internal,
            ..CollectionConfig::default()
        };

        // Rung 1: the device owns the rate; --pps is only a fallback.
        assert_eq!(
            config.resolve_nominal(Some(&status("ping", Some(100)))),
            (Some(100.0), Some(NominalPpsSource::Device)),
            "the device rate outranks --pps, even when the flag is given"
        );

        // Rung 2: unreadable or silent device -> the flag.
        assert_eq!(
            config.resolve_nominal(None),
            (Some(50.0), Some(NominalPpsSource::Argument))
        );
        assert_eq!(
            config.resolve_nominal(Some(&status("ping", None))),
            (Some(50.0), Some(NominalPpsSource::Argument))
        );

        // Nothing declared a rate at all.
        let no_flag = CollectionConfig {
            pps: 0,
            ..config.clone()
        };
        assert_eq!(no_flag.resolve_nominal(None), (None, None));
    }

    #[test]
    fn external_mode_never_consults_the_device_rate() {
        let config = CollectionConfig {
            pps: 20,
            traffic_generator: TrafficGenerator::External,
            ..CollectionConfig::default()
        };

        assert_eq!(
            config.resolve_nominal(Some(&status("external", Some(100)))),
            (Some(20.0), Some(NominalPpsSource::Argument)),
            "the laptop owns the cadence here, so the device rate would describe \
             traffic nobody is sending"
        );

        let no_flag = CollectionConfig { pps: 0, ..config };
        assert_eq!(no_flag.resolve_nominal(None), (None, None));
    }

    // --- D15: the output suffix and its inverse -----------------------------

    #[test]
    fn resolved_output_appends_internal_suffix() {
        let internal = CollectionConfig {
            traffic_generator: TrafficGenerator::Internal,
            ..CollectionConfig::default()
        };
        assert_eq!(
            internal.resolved_output(),
            PathBuf::from("capture_v8_internal.bin")
        );

        let explicit = CollectionConfig {
            output_is_default: false,
            ..internal.clone()
        };
        assert_eq!(explicit.resolved_output(), PathBuf::from(DEFAULT_OUTPUT));

        let external = CollectionConfig {
            traffic_generator: TrafficGenerator::External,
            ..CollectionConfig::default()
        };
        assert_eq!(external.resolved_output(), PathBuf::from(DEFAULT_OUTPUT));

        let no_extension = CollectionConfig {
            output: PathBuf::from("capture_v8"),
            ..internal
        };
        assert_eq!(
            no_extension.resolved_output(),
            PathBuf::from("capture_v8_internal")
        );
    }

    #[test]
    fn traffic_mode_from_filename_is_the_inverse() {
        // Round-trip: every name `resolved_output` can produce reads back as
        // the mode that produced it.
        for output in ["capture_v8.bin", "capture_v8", "run.1.bin"] {
            let internal = CollectionConfig {
                output: PathBuf::from(output),
                traffic_generator: TrafficGenerator::Internal,
                ..CollectionConfig::default()
            };
            let name = internal.resolved_output();
            assert_eq!(
                traffic_mode_from_filename(name.to_str().unwrap()),
                Some(TrafficGenerator::Internal),
                "{output} should round-trip"
            );
        }

        // Negative cases: a name alone proves nothing about an arbitrary file.
        assert_eq!(traffic_mode_from_filename("capture_v8.bin"), None);
        assert_eq!(traffic_mode_from_filename("internal.bin"), None);
        assert_eq!(
            traffic_mode_from_filename("capture_internal_v8.bin"),
            None,
            "the suffix must be terminal"
        );
    }

    #[test]
    fn with_output_clears_the_default_flag() {
        let config = CollectionConfig::with_output("chosen.bin");
        assert!(!config.output_is_default);
        assert_eq!(
            CollectionConfig {
                traffic_generator: TrafficGenerator::Internal,
                ..config
            }
            .resolved_output(),
            PathBuf::from("chosen.bin")
        );
    }

    // --- D3c: a wrong mode is fatal, at the collection boundary -------------

    #[test]
    fn require_mode_mismatch_becomes_a_collection_error() {
        let internal = CollectionConfig {
            traffic_generator: TrafficGenerator::Internal,
            ..CollectionConfig::default()
        };
        let error = device::require_mode(
            Some(&status("external", Some(20))),
            TrafficGenerator::Internal,
        )
        .expect_err("an external device cannot serve an internal capture");
        let converted: CollectionError = error.into();
        assert!(matches!(converted, CollectionError::WrongDeviceMode(_)));
        assert!(
            converted.to_string().contains("--internal"),
            "the message must name the flag that caused it"
        );

        // An unreadable device is unknown, not wrong.
        assert!(
            device::require_mode(None, internal.traffic_generator).is_ok(),
            "a failed read must not block an offline-friendly capture"
        );
    }

    // --- The summary's operator-facing lines --------------------------------

    #[test]
    fn summary_lines_state_the_mode_and_the_rate_source() {
        let summary = CollectionSummary {
            output: PathBuf::from(DEFAULT_OUTPUT),
            bytes_written: 1,
            duration: Duration::from_secs(1),
            configured_pps: 20,
            traffic_mode: TrafficGenerator::Internal,
            device_traffic_mode: Some("ping".to_string()),
            nominal_pps: Some(100.0),
            nominal_pps_source: Some(NominalPpsSource::Device),
            capabilities: "{}".to_string(),
        };
        assert!(summary.device_line().contains("ping"));
        assert!(summary.device_line().contains("internal"));
        assert_eq!(summary.rate_line(), "100 PPS (nominal, from the device)");

        let unread = CollectionSummary {
            device_traffic_mode: None,
            nominal_pps: None,
            nominal_pps_source: None,
            ..summary
        };
        assert!(unread.device_line().contains("unknown"));
        assert!(unread.rate_line().contains("no nominal rate declared"));
    }
}
