//! Read and control the **traffic generator mode** of the RX ESP32.
//!
//! This module is the only place in the crate that speaks to the device about
//! its own state, and the split is deliberate:
//!
//! * [`read`] is a **read-only** `GET /sensing`. Every capture performs it, in
//!   both traffic modes, so the collector knows what the device is doing
//!   instead of assuming it.
//! * [`require_mode`] compares what the device reported with the mode the run
//!   needs, and fails when they disagree. A mismatch means the capture would
//!   contain no CSI at all, so it is reported as an error rather than a
//!   surprise in the analysis stage.
//! * [`set_traffic_generator_mode`] is the **only** write. It is never called
//!   from the capture path: changing the device's mode is an operator action,
//!   exposed by the `device` binary, not a side effect of recording.
//!
//! # Why writes are restricted
//!
//! `traffic_generator_mode` is the one sensing setting with an NVS store, so a
//! write outlives the process that made it. The device's own generator
//! (`ping`) is the factory default and `external` means "send nothing, listen
//! for markers", so a collector that unconditionally patched `external` would
//! leave the device generating no traffic at all after it exits. A capture tool
//! should not mutate the thing it measures.
//!
//! # Example
//!
//! ```no_run
//! use rt_wireless_sensing::device::{self, DeviceEndpoint, TrafficGenerator};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = DeviceEndpoint::new("10.178.22.9", 62587, device::DEFAULT_ORIGIN);
//!
//! // Read-only: what is the device doing right now?
//! if let Some(status) = device::read(&endpoint).await {
//!     println!("mode: {:?}", status.traffic_generator_mode);
//! }
//!
//! // Refuse to capture in a mode the device is not in.
//! device::require_mode(
//!     device::read(&endpoint).await.as_ref(),
//!     TrafficGenerator::Internal,
//! )?;
//! # Ok(())
//! # }
//! ```

use std::error::Error;
use std::fmt;

use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Default HTTP `Origin` header expected by ESPectre.
///
/// Repeated here rather than imported so this module stays usable on its own;
/// [`crate::collection::DEFAULT_ORIGIN`] holds the same value and the two are
/// kept in step by a test.
pub const DEFAULT_ORIGIN: &str = "https://test.espectre.dev";

/// The `traffic_generator_mode` values the device accepts for the *device's own*
/// generator, plus the external-client mode.
///
/// Ordered as documented in `@espectre` `docs/API.md:104`. `Ping` is the
/// factory default.
pub const TRAFFIC_GENERATOR_MODES: [&str; 5] = ["ping", "dns", "dns_tcp", "wifi_raw", "external"];

/// Who produces the traffic a capture records.
///
/// The device reports more than two modes ([`TRAFFIC_GENERATOR_MODES`]); this
/// enum names only the distinction this crate acts on. Everything except
/// `external` means "the device generates its own traffic", which is why the
/// classification is a method ([`DeviceStatus::is_internal`]) rather than a
/// second enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrafficGenerator {
    /// Another host (this crate) sends the traffic; the device sends nothing.
    ///
    /// This is the default so existing commands keep their behaviour.
    #[default]
    External,
    /// The device sends its own traffic (`ping`, `dns`, `dns_tcp`, `wifi_raw`).
    Internal,
}

impl TrafficGenerator {
    /// The `traffic_generator_mode` value this mode corresponds to.
    ///
    /// `Internal` maps to `ping` because that is the device's own default
    /// generator, and the one [`set_traffic_generator_mode`] should restore
    /// when an operator asks for "internal" without naming a packet type.
    pub fn device_mode(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Internal => "ping",
        }
    }

    /// Human-readable name, as serialized into capture metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for TrafficGenerator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the device's HTTP API lives.
///
/// Kept tiny and `Copy` so both capture paths and the `device` binary can share
/// it without threading a whole config through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEndpoint {
    /// ESP32 IPv4 address or host name.
    pub host: String,
    /// TCP port of the ESPectre HTTP API.
    pub api_port: u16,
    /// `Origin` header sent with every request.
    pub origin: String,
}

impl DeviceEndpoint {
    /// Build an endpoint from its parts.
    pub fn new(host: impl Into<String>, api_port: u16, origin: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            api_port,
            origin: origin.into(),
        }
    }

    /// ESPectre API base URL, without a trailing slash.
    pub fn base_url(&self) -> String {
        format!("http://{}:{}/espectre/v1", self.host, self.api_port)
    }
}

/// The subset of `GET /sensing` this crate cares about.
///
/// Every field is `Option` because the read is best-effort: a device that is
/// unreachable, older, or mid-boot must not turn a capture into an error. Only
/// a *successful* read that reports the wrong mode is fatal
/// ([`require_mode`]).
///
/// Fields verified against `@espectre`
/// `src/cpp/frontend/native/espectre/native_direct_frontend.cpp:332-333` and
/// documented at `docs/API.md:102-105`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceStatus {
    /// `traffic_generator_mode`, one of [`TRAFFIC_GENERATOR_MODES`] on a
    /// current firmware. `None` when the field is absent or not a string.
    pub traffic_generator_mode: Option<String>,
    /// `csi_target_pps`, a plain JSON integer (default 100 on Native).
    /// `None` when absent, not an integer, or not positive.
    pub csi_target_pps: Option<u32>,
}

impl DeviceStatus {
    /// Whether the device generates its own traffic.
    ///
    /// `None` when this firmware did not report a mode. Any reported value
    /// other than `external` counts as internal, including modes this crate
    /// does not know (`ping`, `dns`, `dns_tcp`, `wifi_raw`, and any future
    /// one), because all of them mean "the device sends the packets".
    pub fn is_internal(&self) -> Option<bool> {
        self.traffic_generator_mode
            .as_deref()
            .map(|mode| mode != "external")
    }

    /// The mode this status implies, if it reported one.
    pub fn traffic_generator(&self) -> Option<TrafficGenerator> {
        self.is_internal().map(|internal| {
            if internal {
                TrafficGenerator::Internal
            } else {
                TrafficGenerator::External
            }
        })
    }
}

/// Parse a `GET /sensing` body into [`DeviceStatus`].
///
/// Unknown fields are ignored, as `docs/API.md:91` requires of clients, so a
/// firmware that adds sensing fields keeps working. The two fields this crate
/// needs are looked up individually: a body that is valid JSON but has no
/// `traffic_generator_mode` still yields a status in which only
/// [`DeviceStatus::csi_target_pps`] is useful, which is exactly the degradation
/// the ladder needs.
///
/// Returns `None` when the body is not a JSON object at all.
pub fn parse_status(body: &str) -> Option<DeviceStatus> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let object = value.as_object()?;

    Some(DeviceStatus {
        traffic_generator_mode: object
            .get("traffic_generator_mode")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        csi_target_pps: object
            .get("csi_target_pps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pps| u32::try_from(pps).ok())
            .filter(|pps| *pps > 0),
    })
}

/// Read the device status with `GET /sensing`.
///
/// Returns `None` on any transport error, non-2xx status, or unparseable body.
/// Never writes. Callers treat `None` as "unknown", not as a failure: an
/// unreachable device is a warning, because a capture can still be produced and
/// analysed offline.
pub async fn read(endpoint: &DeviceEndpoint) -> Option<DeviceStatus> {
    let base = endpoint.base_url();

    let response = Client::new()
        .get(format!("{base}/sensing"))
        .header("Origin", &endpoint.origin)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;

    let body = response.text().await.ok()?;
    parse_status(&body)
}

/// Failure modes of the device operations.
#[derive(Debug)]
pub enum DeviceError {
    /// A request failed or returned a non-success status.
    Http(reqwest::Error),
    /// The device's mode contradicts the requested one.
    ///
    /// This is not a transport problem: the device answered, and the answer
    /// means the capture would record nothing.
    Mismatch(Box<Mismatch>),
    /// A requested mode is not one the device accepts.
    ///
    /// Rejected locally, before any request is sent, so a typo cannot be
    /// written into the device's NVS store.
    UnknownMode {
        /// The rejected value.
        mode: String,
    },
    /// The write was accepted but the device could not be re-read.
    ///
    /// Distinct from [`Self::Http`] because the outcome is genuinely unknown:
    /// the change may or may not have been applied. Reporting it as a write
    /// failure would be a false claim in the other direction.
    Unconfirmed {
        /// The mode that was written.
        mode: String,
    },
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(f, "device request failed: {error}"),
            Self::Mismatch(mismatch) => write!(f, "{mismatch}"),
            Self::UnknownMode { mode } => write!(
                f,
                "unknown traffic generator mode {mode:?}; expected one of: {}",
                TRAFFIC_GENERATOR_MODES.join(", ")
            ),
            Self::Unconfirmed { mode } => write!(
                f,
                "the device accepted the change to {mode:?} but could not be re-read; \
                 run --show to check the current mode"
            ),
        }
    }
}

impl Error for DeviceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Mismatch(_) | Self::UnknownMode { .. } | Self::Unconfirmed { .. } => None,
        }
    }
}

impl From<reqwest::Error> for DeviceError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

/// The device's reported mode contradicts the requested one.
///
/// Carries enough to explain the problem *and* the fix, so the message and the
/// remedy cannot drift apart: [`Mismatch`]'s `Display` is printed to stderr and
/// [`Mismatch::remedy`] is what the user should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// What the device reported, verbatim (for example `"ping"`).
    pub actual: String,
    /// What the run needs.
    pub requested: TrafficGenerator,
}

impl Mismatch {
    /// The command that puts the device in the requested mode.
    ///
    /// `host` and `api_port` are included so the remedy is copy-pasteable.
    pub fn remedy(&self, host: &str, api_port: u16) -> String {
        format!(
            "cargo run --bin device -- --ip {host} --api-port {api_port} --mode {}",
            self.requested.device_mode()
        )
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.requested {
            TrafficGenerator::Internal => write!(
                f,
                "the device reports traffic_generator_mode: {} but --internal was given, \
                 so the device would not generate the traffic being captured",
                self.actual
            ),
            TrafficGenerator::External => write!(
                f,
                "the device reports traffic_generator_mode: {} but an external capture was \
                 requested, so the device would send its own packets in addition to the \
                 markers",
                self.actual
            ),
        }
    }
}

impl Error for Mismatch {}

/// Check that the device is in the mode the run needs.
///
/// * `None` → `Ok(())`. A device that could not be read is **unknown**, not
///   wrong: the caller warns and continues, because a capture is still usable
///   and refusing to run would make a transient network error fatal.
/// * A reported mode that disagrees with `requested` → [`DeviceError::Mismatch`].
///
/// Callers must treat the error as fatal and stop **before** opening the CSI
/// stream, so that no capture file is created for a run that could not have
/// recorded anything.
pub fn require_mode(
    status: Option<&DeviceStatus>,
    requested: TrafficGenerator,
) -> Result<(), DeviceError> {
    let Some(status) = status else {
        return Ok(());
    };
    let Some(actual) = status.traffic_generator_mode.as_deref() else {
        return Ok(());
    };
    let actual_mode = if actual == "external" {
        TrafficGenerator::External
    } else {
        TrafficGenerator::Internal
    };

    if actual_mode == requested {
        return Ok(());
    }

    Err(DeviceError::Mismatch(Box::new(Mismatch {
        actual: actual.to_string(),
        requested,
    })))
}

/// Write the device's `traffic_generator_mode`, then read it back.
///
/// This is the **only** write in the crate, and nothing in the capture path
/// calls it. It sends the same `PATCH /sensing` body the collector used to send
/// unconditionally, and then re-reads the device instead of trusting the
/// response: the confirmed value is what the device will actually do, and a
/// firmware that silently rejects a value should not look like success.
///
/// `mode` is passed through verbatim after being checked against
/// [`TRAFFIC_GENERATOR_MODES`], so an unknown value is rejected locally rather
/// than sent.
pub async fn set_traffic_generator_mode(
    endpoint: &DeviceEndpoint,
    mode: &str,
) -> Result<DeviceStatus, DeviceError> {
    if !TRAFFIC_GENERATOR_MODES.contains(&mode) {
        return Err(DeviceError::UnknownMode {
            mode: mode.to_string(),
        });
    }

    let base = endpoint.base_url();

    // The write itself. A failure here is reported verbatim: it means the
    // device rejected or never received the change, not that it is unreadable.
    Client::new()
        .patch(format!("{base}/sensing"))
        .header("Origin", &endpoint.origin)
        .json(&serde_json::json!({ "traffic_generator_mode": mode }))
        .send()
        .await?
        .error_for_status()?;

    // Read-after-write: the PATCH response is an acknowledgement, not proof.
    // A failed re-read is its own error, because "wrote it but could not
    // confirm" must not be reported as "the write did not take effect".
    read(endpoint).await.ok_or(DeviceError::Unconfirmed {
        mode: mode.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `GET /sensing` body shaped like the firmware's, per `docs/API.md:94-107`.
    const SENSING_BODY: &str = r#"{
        "enabled": true,
        "ready": false,
        "calibrating": false,
        "mode": "csi_collection",
        "derived_events_paused": true,
        "detector": "lightweight",
        "threshold": 0.5,
        "motion_on_hits": 3,
        "motion_off_hits": 5,
        "traffic_generator_mode": "ping",
        "csi_target_pps": 100,
        "csi_traffic_udp_port": 5555
    }"#;

    #[test]
    fn device_sensing_parser() {
        let status = parse_status(SENSING_BODY).expect("valid body");
        assert_eq!(status.traffic_generator_mode.as_deref(), Some("ping"));
        assert_eq!(status.csi_target_pps, Some(100));
        assert_eq!(status.is_internal(), Some(true));
    }

    #[test]
    fn missing_pps_still_parses_the_mode() {
        let body = r#"{"traffic_generator_mode": "external"}"#;
        let status = parse_status(body).expect("valid body");
        assert_eq!(status.traffic_generator_mode.as_deref(), Some("external"));
        assert_eq!(status.csi_target_pps, None);
        assert_eq!(status.is_internal(), Some(false));
    }

    #[test]
    fn non_positive_or_non_integer_pps_is_none() {
        for body in [
            r#"{"csi_target_pps": 0}"#,
            r#"{"csi_target_pps": -5}"#,
            r#"{"csi_target_pps": "100"}"#,
            r#"{"csi_target_pps": 100.5}"#,
        ] {
            let status = parse_status(body).expect("valid body");
            assert_eq!(status.csi_target_pps, None, "body: {body}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // `docs/API.md:91`: clients must ignore fields they do not know.
        let body = r#"{"traffic_generator_mode": "dns", "csi_target_pps": 50,
                       "future_field": {"nested": [1, 2, 3]}}"#;
        let status = parse_status(body).expect("valid body");
        assert_eq!(status.traffic_generator_mode.as_deref(), Some("dns"));
        assert_eq!(status.csi_target_pps, Some(50));
    }

    #[test]
    fn non_object_body_is_none() {
        assert_eq!(parse_status("not json"), None);
        assert_eq!(parse_status("[1, 2, 3]"), None);
        assert_eq!(parse_status(r#""ping""#), None);
    }

    #[test]
    fn every_non_external_mode_counts_as_internal() {
        for mode in ["ping", "dns", "dns_tcp", "wifi_raw"] {
            let status = DeviceStatus {
                traffic_generator_mode: Some(mode.to_string()),
                csi_target_pps: None,
            };
            assert_eq!(status.is_internal(), Some(true), "mode: {mode}");
            assert_eq!(
                status.traffic_generator(),
                Some(TrafficGenerator::Internal),
                "mode: {mode}"
            );
        }
    }

    #[test]
    fn wrong_device_mode_is_fatal() {
        // The full matrix from the plan's D3c.
        let cases = [
            ("external", TrafficGenerator::Internal, false),
            ("ping", TrafficGenerator::External, false),
            ("dns", TrafficGenerator::External, false),
            ("wifi_raw", TrafficGenerator::External, false),
            ("ping", TrafficGenerator::Internal, true),
            ("external", TrafficGenerator::External, true),
        ];

        for (actual, requested, should_pass) in cases {
            let status = DeviceStatus {
                traffic_generator_mode: Some(actual.to_string()),
                csi_target_pps: Some(100),
            };
            let result = require_mode(Some(&status), requested);
            assert_eq!(
                result.is_ok(),
                should_pass,
                "actual={actual} requested={requested:?}"
            );
        }
    }

    #[test]
    fn unknown_device_mode_never_blocks_a_run() {
        // An unreadable device, or one that reports no mode, is unknown, not wrong.
        assert!(require_mode(None, TrafficGenerator::Internal).is_ok());
        assert!(require_mode(None, TrafficGenerator::External).is_ok());

        let no_mode = DeviceStatus {
            traffic_generator_mode: None,
            csi_target_pps: Some(100),
        };
        assert!(require_mode(Some(&no_mode), TrafficGenerator::Internal).is_ok());
    }

    #[test]
    fn remedy_names_the_right_mode() {
        let to_ping = Mismatch {
            actual: "external".to_string(),
            requested: TrafficGenerator::Internal,
        };
        let remedy = to_ping.remedy("10.0.0.5", 62587);
        assert!(remedy.contains("--mode ping"), "remedy: {remedy}");
        assert!(remedy.contains("--ip 10.0.0.5"), "remedy: {remedy}");
        assert!(remedy.contains("--api-port 62587"), "remedy: {remedy}");

        let to_external = Mismatch {
            actual: "ping".to_string(),
            requested: TrafficGenerator::External,
        };
        let remedy = to_external.remedy("10.0.0.5", 62587);
        assert!(remedy.contains("--mode external"), "remedy: {remedy}");
    }

    #[test]
    fn unconfirmed_is_distinct_from_a_write_failure() {
        // "Wrote it but could not confirm" must not read like "the write
        // failed": the operator's next step differs.
        let unconfirmed = DeviceError::Unconfirmed {
            mode: "ping".to_string(),
        };
        let message = unconfirmed.to_string();
        assert!(message.contains("accepted"), "message: {message}");
        assert!(
            message.contains("could not be re-read"),
            "message: {message}"
        );
        assert!(message.contains("--show"), "message: {message}");
        assert!(
            !message.contains("did not take effect"),
            "message: {message}"
        );
    }

    #[test]
    fn mismatch_message_names_both_sides() {
        let mismatch = Mismatch {
            actual: "external".to_string(),
            requested: TrafficGenerator::Internal,
        };
        let message = mismatch.to_string();
        assert!(message.contains("external"), "message: {message}");
        assert!(message.contains("--internal"), "message: {message}");

        let mismatch = Mismatch {
            actual: "ping".to_string(),
            requested: TrafficGenerator::External,
        };
        let message = mismatch.to_string();
        assert!(message.contains("ping"), "message: {message}");
        assert!(message.contains("markers"), "message: {message}");
    }

    #[test]
    fn mode_round_trips_through_its_name() {
        assert_eq!(TrafficGenerator::External.device_mode(), "external");
        assert_eq!(TrafficGenerator::Internal.device_mode(), "ping");
        assert_eq!(TrafficGenerator::External.as_str(), "external");
        assert_eq!(TrafficGenerator::Internal.as_str(), "internal");
        assert_eq!(TrafficGenerator::default(), TrafficGenerator::External);
    }

    #[test]
    fn base_url_has_no_trailing_slash() {
        let endpoint = DeviceEndpoint::new("10.178.22.9", 62587, DEFAULT_ORIGIN);
        assert_eq!(endpoint.base_url(), "http://10.178.22.9:62587/espectre/v1");
    }

    #[test]
    fn default_origin_matches_the_collection_module() {
        assert_eq!(crate::collection::DEFAULT_ORIGIN, DEFAULT_ORIGIN);
    }

    #[test]
    fn every_documented_mode_is_recognized() {
        // `@espectre` `docs/API.md:104`, plus `external`.
        assert_eq!(TRAFFIC_GENERATOR_MODES.len(), 5);
        assert!(TRAFFIC_GENERATOR_MODES.contains(&"ping"));
        assert!(TRAFFIC_GENERATOR_MODES.contains(&"external"));
    }

    /// Serve exactly `count` requests sequentially, replying with `status` and
    /// `body` to each.
    ///
    /// The listener is moved into the server thread, which does its own
    /// blocking `accept`: accepting on the test thread would hold the runtime
    /// and prevent the client from ever connecting.
    ///
    /// Returns a handle to the joined request heads, so the caller can assert
    /// on method and path. A real socket is used rather than a mock HTTP layer
    /// because the point is the wire behaviour of [`read`] and
    /// [`set_traffic_generator_mode`], not their parsing alone.
    fn serve(
        listener: std::net::TcpListener,
        status: &str,
        body: &str,
        count: usize,
    ) -> std::thread::JoinHandle<Vec<String>> {
        use std::io::{Read, Write};

        let status = status.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            let mut heads = Vec::with_capacity(count);
            for _ in 0..count {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buffer = [0u8; 8192];
                // Read the request head only; the client sends no body we need.
                let read = stream.read(&mut buffer).expect("read request");
                heads.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("write reply");
                stream.flush().expect("flush");
            }
            heads
        })
    }

    #[test]
    fn read_returns_the_parsed_status() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = serve(listener, "200 OK", SENSING_BODY, 1);

        let endpoint = DeviceEndpoint::new("127.0.0.1", port, DEFAULT_ORIGIN);
        let status = block_on(read(&endpoint)).expect("status");

        assert_eq!(status.traffic_generator_mode.as_deref(), Some("ping"));
        assert_eq!(status.csi_target_pps, Some(100));

        let request = &server.join().expect("server")[0];
        assert!(request.starts_with("GET /espectre/v1/sensing"), "{request}");
        assert!(
            request.contains(&format!("origin: {DEFAULT_ORIGIN}"))
                || request.contains(&format!("Origin: {DEFAULT_ORIGIN}")),
            "{request}"
        );
    }

    #[test]
    fn read_is_none_on_a_non_success_status() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = serve(listener, "500 Internal Server Error", "{}", 1);

        let endpoint = DeviceEndpoint::new("127.0.0.1", port, DEFAULT_ORIGIN);
        assert_eq!(block_on(read(&endpoint)), None);
        server.join().expect("server");
    }

    #[test]
    fn set_patches_then_re_reads() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();

        // Exchange 1 is the PATCH; exchange 2 is the confirming re-read, which
        // reports the new mode. Both are served by one thread, in order.
        let confirmed = SENSING_BODY.replace("\"ping\"", "\"external\"");
        let server = serve(listener, "200 OK", &confirmed, 2);

        let endpoint = DeviceEndpoint::new("127.0.0.1", port, DEFAULT_ORIGIN);
        let status = block_on(set_traffic_generator_mode(&endpoint, "external")).expect("status");

        let heads = server.join().expect("server");

        assert!(
            heads[0].starts_with("PATCH /espectre/v1/sensing"),
            "{}",
            heads[0]
        );
        assert!(
            heads[0].contains("\"traffic_generator_mode\":\"external\""),
            "{}",
            heads[0]
        );
        assert!(
            heads[1].starts_with("GET /espectre/v1/sensing"),
            "{}",
            heads[1]
        );

        // The returned status is the re-read, not the PATCH response.
        assert_eq!(status.traffic_generator_mode.as_deref(), Some("external"));
    }

    #[test]
    fn set_rejects_an_unknown_mode_without_sending_anything() {
        // Nothing should ever connect to this listener.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking probe allowed");

        let endpoint = DeviceEndpoint::new("127.0.0.1", port, DEFAULT_ORIGIN);
        let error = block_on(set_traffic_generator_mode(&endpoint, "pacing"))
            .expect_err("unknown mode must be rejected");

        assert!(
            matches!(error, DeviceError::UnknownMode { .. }),
            "{error:?}"
        );
        assert!(
            listener.accept().is_err(),
            "an unknown mode must not open a connection"
        );
    }

    /// Run a future to completion on the current thread.
    ///
    /// These tests need a real socket. A tiny local runtime keeps the
    /// dependency surface unchanged: `#[tokio::test]` would pull the macro's
    /// features into every build, while `tokio` itself is already a dependency.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
