//! Real-time CSI collection with a live amplitude-heatmap dashboard.
//!
//! This module composes the three lower-level pieces:
//!
//! * [`crate::collection`] records the raw ESPectre stream to a `.bin` file.
//! * [`crate::decoder`] incrementally turns that stream into [`CsiPacket`]s.
//! * [`crate::visualization`] renders the packet window and serves the web
//!   dashboard.
//!
//! Only the most recent [`StreamingConfig::window`] of device time is kept and
//! published every [`StreamingConfig::update_interval`]. The dashboard stays in
//! a waiting state for [`StreamingConfig::warmup`] before the first heatmap.
//!
//! # Example
//!
//! ```no_run
//! use rt_wireless_sensing::streaming::{StreamingConfig, run};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = StreamingConfig {
//!     port: 8080,
//!     ..StreamingConfig::default()
//! };
//! let summary = run(&config).await?;
//! println!("decoded {} records", summary.records_decoded);
//! # Ok(())
//! # }
//! ```

use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use tokio::{net::TcpListener, sync::mpsc, task::JoinError, time::interval};

use crate::{
    collection::{self, CollectionConfig, CollectionError, CollectionSummary},
    decoder::{CsiPacket, DecodeError, FrameStreamDecoder},
    visualization::{
        self, LiveDashboardSnapshot, VisualizationError, VisualizationOptions,
        prepare_visualization_data_with_options,
    },
};

/// Default dashboard port.
pub const DEFAULT_PORT: u16 = 8080;

/// Default waiting period before the first heatmap.
pub const DEFAULT_WARMUP: Duration = Duration::from_secs(3);

/// Default publish interval for the dashboard data.
pub const DEFAULT_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

/// Default maximum time span shown in the heatmap.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30);

/// Settings for a live streaming run.
///
/// The collection settings (device address, PPS, output path, ...) are nested
/// in [`StreamingConfig::collection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingConfig {
    /// Collection settings for the raw `.bin` capture.
    pub collection: CollectionConfig,
    /// Port of the live dashboard on `127.0.0.1`.
    pub port: u16,
    /// Time before the first heatmap is published.
    pub warmup: Duration,
    /// How often the published dashboard data is refreshed.
    pub update_interval: Duration,
    /// Maximum amount of device time retained and displayed.
    pub window: Duration,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            // Per-chunk progress is noisy for a live dashboard; the server URL
            // and final summary are enough by default.
            collection: CollectionConfig {
                verbose: false,
                ..CollectionConfig::default()
            },
            port: DEFAULT_PORT,
            warmup: DEFAULT_WARMUP,
            update_interval: DEFAULT_UPDATE_INTERVAL,
            window: DEFAULT_WINDOW,
        }
    }
}

impl StreamingConfig {
    /// Address the dashboard listens on.
    pub fn bind_addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Reject settings that would panic or produce an unusable dashboard.
    fn validate(&self) -> Result<(), StreamingError> {
        if self.update_interval.is_zero() {
            return Err(StreamingError::InvalidConfig(
                "update_interval must be greater than zero".to_string(),
            ));
        }
        if self.window.is_zero() {
            return Err(StreamingError::InvalidConfig(
                "window must be greater than zero".to_string(),
            ));
        }
        if self.collection.pps == 0 {
            return Err(StreamingError::InvalidConfig(
                "pps must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

/// Result of a completed or interrupted streaming run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingSummary {
    /// Raw capture file written during the run.
    pub output: PathBuf,
    /// Total CSI records decoded, including records already outside the window.
    pub records_decoded: u64,
    /// Records in the final published window.
    pub window_records: usize,
    /// Size of the raw capture after streaming stopped.
    pub bytes_written: u64,
    /// Wall-clock duration of the run.
    pub duration: Duration,
}

/// Failure modes of [`run`].
#[derive(Debug)]
pub enum StreamingError {
    /// The collection task failed.
    Collection(CollectionError),
    /// The stream could not be decoded.
    Decode(DecodeError),
    /// Data preparation or the dashboard server failed.
    Visualization(VisualizationError),
    /// Binding the dashboard listener or reading the capture metadata failed.
    Io(std::io::Error),
    /// A background task panicked or was cancelled unexpectedly.
    Join(JoinError),
    /// The shared dashboard state lock was poisoned.
    State(String),
    /// A [`StreamingConfig`] value is unusable.
    InvalidConfig(String),
    /// The dashboard server stopped unexpectedly.
    Server(String),
}

impl fmt::Display for StreamingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collection(error) => write!(f, "collection failed: {error}"),
            Self::Decode(error) => write!(f, "decode failed: {error}"),
            Self::Visualization(error) => write!(f, "visualization failed: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Join(error) => write!(f, "background task failed: {error}"),
            Self::State(message) => write!(f, "dashboard state error: {message}"),
            Self::InvalidConfig(message) => write!(f, "invalid streaming configuration: {message}"),
            Self::Server(message) => write!(f, "dashboard server error: {message}"),
        }
    }
}

impl Error for StreamingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Collection(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Visualization(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Join(error) => Some(error),
            Self::State(_) | Self::InvalidConfig(_) | Self::Server(_) => None,
        }
    }
}

impl From<CollectionError> for StreamingError {
    fn from(error: CollectionError) -> Self {
        Self::Collection(error)
    }
}

impl From<DecodeError> for StreamingError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<VisualizationError> for StreamingError {
    fn from(error: VisualizationError) -> Self {
        Self::Visualization(error)
    }
}

impl From<std::io::Error> for StreamingError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<JoinError> for StreamingError {
    fn from(error: JoinError) -> Self {
        Self::Join(error)
    }
}

/// Collect the CSI stream, decode it live, and serve the sliding-window
/// dashboard.
///
/// The function returns when Ctrl+C is pressed or the collection stream ends.
/// The raw capture written by [`StreamingConfig::collection`] is left on disk.
pub async fn run(config: &StreamingConfig) -> Result<StreamingSummary, StreamingError> {
    config.validate()?;
    let started = Instant::now();

    let state = Arc::new(RwLock::new(LiveDashboardSnapshot::Waiting {
        elapsed_seconds: 0.0,
        warmup_seconds: config.warmup.as_secs_f64(),
    }));

    // Bind before starting collection so a busy port fails fast and no markers
    // are sent for a dashboard that cannot start.
    let listener = TcpListener::bind(config.bind_addr()).await?;
    let mut server_task = tokio::spawn(visualization::serve_dashboard_live_on(
        listener,
        Arc::clone(&state),
    ));

    let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let collection_config = config.collection.clone();
    let mut collection_task = tokio::spawn(async move {
        collection::collect_with_sink(&collection_config, move |chunk| {
            let _ = chunk_tx.send(chunk.to_vec());
        })
        .await
    });

    let accumulator = LiveAccumulator::new(config, Arc::clone(&state), started);
    let update_interval = config.update_interval;
    let mut decoder_task = tokio::spawn(run_decoder_loop(chunk_rx, accumulator, update_interval));

    let outcome = tokio::select! {
        result = &mut collection_task => {
            RunOutcome::Collection(join_collection(result))
        }
        result = &mut decoder_task => {
            RunOutcome::Decoder(join_decoder(result))
        }
        result = &mut server_task => {
            RunOutcome::Server(join_server(result))
        }
        _ = tokio::signal::ctrl_c() => RunOutcome::Interrupted,
    };

    match outcome {
        RunOutcome::Collection(result) => {
            // The collection future dropped its chunk sender, so the decoder
            // drains the remaining chunks and publishes the final window.
            let decoder_summary = decoder_task.await?;
            server_task.abort();
            let _ = server_task.await;

            let decoder_summary = decoder_summary?;
            let summary = build_summary(config, &decoder_summary, started);
            result?;
            Ok(summary)
        }
        RunOutcome::Decoder(result) => {
            let decoder_summary = match result {
                Ok(summary) => summary,
                Err(error) => {
                    collection_task.abort();
                    let _ = collection_task.await;
                    server_task.abort();
                    let _ = server_task.await;
                    return Err(error);
                }
            };

            // A clean decoder exit means the chunk channel closed because the
            // collection task finished. Wait for its result as well.
            let collection_result = collection_task.await?;
            server_task.abort();
            let _ = server_task.await;

            let summary = build_summary(config, &decoder_summary, started);
            collection_result?;
            Ok(summary)
        }
        RunOutcome::Server(result) => {
            collection_task.abort();
            let _ = collection_task.await;
            decoder_task.abort();
            let _ = decoder_task.await;

            result?;
            Err(StreamingError::Server(
                "live dashboard server stopped unexpectedly".to_string(),
            ))
        }
        RunOutcome::Interrupted => {
            // Aborting the collection future drops the chunk sender. Let the
            // decoder drain and publish once more before shutting the server.
            collection_task.abort();
            let _ = collection_task.await;
            let decoder_summary = decoder_task.await?;
            server_task.abort();
            let _ = server_task.await;

            let decoder_summary = decoder_summary?;
            Ok(build_summary(config, &decoder_summary, started))
        }
    }
}

/// Outcome of the first task to finish.
enum RunOutcome {
    Collection(Result<CollectionSummary, StreamingError>),
    Decoder(Result<DecoderLoopSummary, StreamingError>),
    Server(Result<(), StreamingError>),
    Interrupted,
}

fn join_collection(
    result: Result<Result<CollectionSummary, CollectionError>, JoinError>,
) -> Result<CollectionSummary, StreamingError> {
    match result {
        Ok(inner) => inner.map_err(StreamingError::Collection),
        Err(error) => Err(StreamingError::Join(error)),
    }
}

fn join_decoder(
    result: Result<Result<DecoderLoopSummary, StreamingError>, JoinError>,
) -> Result<DecoderLoopSummary, StreamingError> {
    match result {
        Ok(inner) => inner,
        Err(error) => Err(StreamingError::Join(error)),
    }
}

fn join_server(
    result: Result<Result<(), VisualizationError>, JoinError>,
) -> Result<(), StreamingError> {
    match result {
        Ok(inner) => inner.map_err(StreamingError::Visualization),
        Err(error) => Err(StreamingError::Join(error)),
    }
}

fn build_summary(
    config: &StreamingConfig,
    decoder: &DecoderLoopSummary,
    started: Instant,
) -> StreamingSummary {
    let bytes_written = std::fs::metadata(&config.collection.output)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    StreamingSummary {
        output: config.collection.output.clone(),
        records_decoded: decoder.records_decoded,
        window_records: decoder.window_records,
        bytes_written,
        duration: started.elapsed(),
    }
}

/// Counters returned by the decoder task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DecoderLoopSummary {
    records_decoded: u64,
    window_records: usize,
}

/// Incremental decode state shared by the decoder task.
struct LiveAccumulator {
    decoder: FrameStreamDecoder,
    packets: Vec<CsiPacket>,
    origin_ticks: Option<u64>,
    source: Option<String>,
    nominal_pps: f64,
    window_us: u64,
    warmup: Duration,
    started: Instant,
    state: Arc<RwLock<LiveDashboardSnapshot>>,
    total_decoded: u64,
}

impl LiveAccumulator {
    fn new(
        config: &StreamingConfig,
        state: Arc<RwLock<LiveDashboardSnapshot>>,
        started: Instant,
    ) -> Self {
        let source = config
            .collection
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        let window_us = u64::try_from(config.window.as_micros()).unwrap_or(u64::MAX);

        Self {
            decoder: FrameStreamDecoder::new(),
            packets: Vec::new(),
            origin_ticks: None,
            source,
            nominal_pps: config.collection.pps as f64,
            window_us,
            warmup: config.warmup,
            started,
            state,
            total_decoded: 0,
        }
    }

    fn ingest(&mut self, chunk: &[u8]) -> Result<(), StreamingError> {
        self.decoder.push(chunk);
        while let Some(packet) = self.decoder.next_frame()? {
            if self.origin_ticks.is_none() {
                self.origin_ticks = Some(packet.csi.device_ticks_us);
            }
            self.packets.push(packet);
            self.total_decoded += 1;
        }
        Ok(())
    }

    fn publish(&mut self) -> Result<(), StreamingError> {
        let elapsed = self.started.elapsed();

        // No heatmap during warm-up, and never publish an empty frame.
        if elapsed < self.warmup || self.packets.is_empty() {
            self.set_state(LiveDashboardSnapshot::Waiting {
                elapsed_seconds: elapsed.as_secs_f64(),
                warmup_seconds: self.warmup.as_secs_f64(),
            })?;
            return Ok(());
        }

        let latest_ticks = self
            .packets
            .last()
            .map_or(0, |packet| packet.csi.device_ticks_us);
        retain_last_window(&mut self.packets, latest_ticks, self.window_us, |packet| {
            packet.csi.device_ticks_us
        });

        let origin_ticks = self.origin_ticks.ok_or_else(|| {
            StreamingError::State("origin timestamp missing from live buffer".to_string())
        })?;

        // The published window is a sliding slice, so its last record is not
        // the end of the capture: the diagnostic trace must treat the trailing
        // slots as a window edge, not as a capture edge.
        let data = prepare_visualization_data_with_options(
            &self.packets,
            self.source.as_deref(),
            origin_ticks,
            VisualizationOptions {
                nominal_pps: Some(self.nominal_pps),
                capture_end: false,
            },
        )?;

        self.set_state(LiveDashboardSnapshot::Ready {
            data: Box::new(data),
        })
    }

    fn set_state(&self, snapshot: LiveDashboardSnapshot) -> Result<(), StreamingError> {
        let mut guard = self
            .state
            .write()
            .map_err(|_| StreamingError::State("live dashboard state lock poisoned".to_string()))?;
        *guard = snapshot;
        Ok(())
    }
}

/// Consume raw chunks, decode them, and publish a snapshot every 500 ms.
async fn run_decoder_loop(
    mut chunks: mpsc::UnboundedReceiver<Vec<u8>>,
    mut accumulator: LiveAccumulator,
    update_interval: Duration,
) -> Result<DecoderLoopSummary, StreamingError> {
    let mut ticker = interval(update_interval);

    loop {
        tokio::select! {
            chunk = chunks.recv() => match chunk {
                Some(chunk) => accumulator.ingest(&chunk)?,
                None => break,
            },
            _ = ticker.tick() => accumulator.publish()?,
        }
    }

    // A Ctrl+C can stop collection in the middle of a frame. The raw capture
    // keeps whatever was written; the live view simply ignores that tail.
    if let Err(error) = accumulator.decoder.finish() {
        eprintln!("streaming: ignoring incomplete final frame: {error}");
    }

    accumulator.publish()?;

    Ok(DecoderLoopSummary {
        records_decoded: accumulator.total_decoded,
        window_records: accumulator.packets.len(),
    })
}

/// Keep only records at or after `latest_ticks - window_us`.
///
/// `items` must be ordered by the tick value returned by `ticks` (the decoder
/// preserves stream order).
fn retain_last_window<T>(
    items: &mut Vec<T>,
    latest_ticks: u64,
    window_us: u64,
    ticks: impl Fn(&T) -> u64,
) {
    let cutoff = latest_ticks.saturating_sub(window_us);
    let keep_from = items.partition_point(|item| ticks(item) < cutoff);
    items.drain(..keep_from);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_the_documented_defaults() {
        let config = StreamingConfig::default();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.warmup, DEFAULT_WARMUP);
        assert_eq!(config.update_interval, DEFAULT_UPDATE_INTERVAL);
        assert_eq!(config.window, DEFAULT_WINDOW);
        assert!(!config.collection.verbose);
        assert_eq!(config.bind_addr(), "127.0.0.1:8080");
    }

    #[test]
    fn zero_interval_or_window_is_rejected() {
        let config = StreamingConfig {
            update_interval: Duration::ZERO,
            ..StreamingConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(StreamingError::InvalidConfig(_))
        ));

        let config = StreamingConfig {
            window: Duration::ZERO,
            ..StreamingConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(StreamingError::InvalidConfig(_))
        ));
    }

    #[test]
    fn window_retains_only_recent_ticks() {
        let mut items: Vec<u64> = (0..=32_000_000).step_by(50_000).collect();
        retain_last_window(&mut items, 32_000_000, 30_000_000, |value| *value);
        assert_eq!(items.first().copied(), Some(2_000_000));
        assert_eq!(items.last().copied(), Some(32_000_000));
        assert!(items.len() <= 601);
    }

    #[test]
    fn empty_window_retains_only_the_latest_tick() {
        let mut items = vec![0, 1_000, 2_000];
        retain_last_window(&mut items, 2_000, 0, |value| *value);
        assert_eq!(items, vec![2_000]);
    }
}
