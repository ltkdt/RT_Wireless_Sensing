//! CSI stream collection from an ESPectre device.
//!
//! This module owns the network side of the pipeline: it talks to the ESP32's
//! ESPectre HTTP API, enables external-traffic sensing, feeds the device with
//! UDP markers at the requested packet rate, and writes the raw CSI response
//! body to a binary file. It deliberately does **not** decode the bytes; use
//! [`crate::decoder`] for that and [`crate::saving`] to produce numeric arrays.
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
use tokio::{
    io::AsyncWriteExt,
    net::UdpSocket,
    time::{interval, sleep},
};

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
    pub marker_port: u16,
    /// Marker rate; must be greater than zero.
    pub pps: u32,
    /// `Origin` header sent with every HTTP request.
    pub origin: String,
    /// Destination of the raw CSI byte stream. Overwritten if it exists.
    pub output: PathBuf,
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
            origin: DEFAULT_ORIGIN.to_string(),
            output: PathBuf::from(DEFAULT_OUTPUT),
            startup_delay: Duration::from_millis(200),
            max_duration: None,
            verbose: true,
        }
    }
}

impl CollectionConfig {
    /// Build a configuration that writes to `output` and uses the other
    /// defaults.
    pub fn with_output(output: impl Into<PathBuf>) -> Self {
        Self {
            output: output.into(),
            ..Self::default()
        }
    }

    /// ESPectre API base URL derived from [`Self::esp32_ip`] and
    /// [`Self::api_port`].
    pub fn base_url(&self) -> String {
        format!("http://{}:{}/espectre/v1", self.esp32_ip, self.api_port)
    }

    /// Marker period implied by [`Self::pps`], clamped to at least 1 ms.
    pub fn marker_period(&self) -> Duration {
        Duration::from_millis((1_000 / u64::from(self.pps.max(1))).max(1))
    }

    /// Address that receives the UDP markers, which is [`Self::esp32_ip`] and [`Self::marker_port`].
    pub fn marker_target(&self) -> Result<SocketAddr, CollectionError> {
        let endpoint = format!("{}:{}", self.esp32_ip, self.marker_port);
        endpoint
            .parse()
            .map_err(|source| CollectionError::InvalidEndpoint {
                endpoint: endpoint.clone(),
                source,
            })
    }

    /// Reject settings that would fail later in the run.
    fn validate(&self) -> Result<(), CollectionError> {
        if self.pps == 0 {
            return Err(CollectionError::InvalidPps);
        }
        self.marker_target()?;
        Ok(())
    }
}

/// What a successful collection run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSummary {
    /// File the raw stream was written to.
    pub output: PathBuf,
    /// Total number of bytes written.
    pub bytes_written: u64,
    /// Wall-clock duration of the streaming phase.
    pub duration: Duration,
    /// Configured marker rate.
    pub configured_pps: u32,
    /// Raw JSON text returned by `GET /capabilities`.
    pub capabilities: String,
    /// Raw JSON text returned by `PATCH /sensing`.
    pub sensing: String,
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
/// The function performs the same sequence as ESPectre's documented workflow:
///
/// 1. `GET /capabilities` to confirm the endpoint is reachable.
/// 2. `PATCH /sensing` with `{"traffic_generator_mode": "external"}`.
/// 3. Spawn a UDP marker sender at [`CollectionConfig::pps`].
/// 4. Wait [`CollectionConfig::startup_delay`].
/// 5. `GET /csi` and copy every response chunk verbatim into the output file.
///
/// The marker task is stopped when the stream ends or an error occurs. The
/// returned [`CollectionSummary`] contains the capabilities and sensing
/// responses as well as byte and duration counters.
///
/// This is the convenience form of [`collect_with_sink`] with a no-op chunk
/// callback.
///
/// # Cancellation
///
/// This future completes when the HTTP body ends (the device closes the
/// stream), when [`CollectionConfig::max_duration`] elapses, or on an
/// I/O/HTTP error. Dropping the future aborts the HTTP request and the
/// marker task.
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

    // 2. Enable external traffic mode.
    let sensing = client
        .patch(format!("{base}/sensing"))
        .header("Origin", &config.origin)
        .json(&serde_json::json!({
            "traffic_generator_mode": "external"
        }))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // 3. Send UDP markers at the configured rate. The guard aborts the task
    //    when it is dropped, including when this future is cancelled.
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let sender = Arc::clone(&socket);
    let target = config.marker_target()?;
    let period = config.marker_period();
    let marker_task = AbortOnDrop(tokio::spawn(async move {
        let mut timer = interval(period);
        loop {
            timer.tick().await;
            if sender.send_to(&TRAFFIC_MARKER, target).await.is_err() {
                break;
            }
        }
    }));

    // 4. Give the ESP32 time to start sampling before opening the stream.
    sleep(config.startup_delay).await;

    // 5. Stream the response body into the output file.
    let streaming = stream_to_file(config, &client, &base, &mut on_chunk).await;

    drop(marker_task);
    let (bytes_written, duration) = streaming?;

    Ok(CollectionSummary {
        output: config.output.clone(),
        bytes_written,
        duration,
        configured_pps: config.pps,
        capabilities,
        sensing,
    })
}

/// Open `/csi` and copy the response body into the configured file.
///
/// After each chunk is written, `on_chunk` is called with the same bytes.
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
    let mut file = tokio::fs::File::create(&config.output).await?;
    let mut total = 0u64;
    let mut deadline = config.max_duration.map(|max| Box::pin(sleep(max)));

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
        file.write_all(&chunk).await?;
        total += chunk.len() as u64;
        on_chunk(&chunk);
        if config.verbose {
            eprintln!("Received {total} bytes");
        }
    }

    file.flush().await?;

    Ok((total, started.elapsed()))
}

/// Return the conventional output path for a capture.
///
/// This is only a naming helper; collection itself writes the path chosen in
/// [`CollectionConfig::output`].
pub fn default_output_path() -> &'static Path {
    Path::new(DEFAULT_OUTPUT)
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
}
