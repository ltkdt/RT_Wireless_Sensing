//! Default binary: collect the ESPectre CSI stream.
//!
//! `main` is intentionally a thin wrapper around
//! [`rt_wireless_sensing::collection::collect`]. The default configuration
//! reproduces the original streamer: ESP32 at `10.178.22.9`, 20 PPS markers,
//! and `capture_v8.bin` in the current directory.
//!
//! The only option is `--internal`, which stops this binary from sending the
//! UDP markers so the device can generate its own traffic:
//!
//! ```text
//! cargo run --                        # external: this host sends the markers
//! cargo run -- --internal             # internal: the device sends the traffic
//! ```
//!
//! For the full option set, decoding, or array export use the companion
//! binaries:
//!
//! ```text
//! cargo run --bin collect -- --help
//! cargo run --bin decode -- capture_v8.bin
//! cargo run --bin save -- capture_v8.bin
//! ```

use std::{env, process::ExitCode};

use rt_wireless_sensing::collection::{self, CollectionConfig, CollectionError, TrafficGenerator};

/// Exit code used when the device is in the wrong traffic mode (D3c).
const EXIT_WRONG_MODE: u8 = 3;

#[tokio::main]
async fn main() -> ExitCode {
    let config = match parse_args(env::args().skip(1)) {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!("{}", USAGE);
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match collection::collect(&config).await {
        Ok(summary) => {
            println!("Capabilities: {}", summary.capabilities);
            println!("device       : {}", summary.device_line());
            println!("rate         : {}", summary.rate_line());
            println!(
                "Saved {} bytes to {} in {:.3} s at {} PPS",
                summary.bytes_written,
                summary.output.display(),
                summary.duration.as_secs_f64(),
                summary.configured_pps
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            match error {
                CollectionError::WrongDeviceMode(_) => ExitCode::from(EXIT_WRONG_MODE),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

/// Printed for `--help` and for bad arguments.
const USAGE: &str = "\
collect a raw ESPectre V8 CSI stream

usage: rt-wireless-sensing [--internal]

options:
  --internal  do not send traffic markers: the device generates its own traffic
  -h, --help  show this help

Without --internal this binary sends the UDP markers (external mode) and needs
the device in `traffic_generator_mode: external`. With --internal nothing is
sent and the device must be generating its own traffic. A device in the wrong
mode stops the run with exit code 3 and no capture file. The capture never
changes the device's mode.

For the full option set use `cargo run --bin collect -- --help`.
";

/// Parse `--internal` and `-h`/`--help`.
///
/// Returns `Ok(None)` for a request to print usage. An unknown argument is an
/// error rather than being ignored: this binary accepted nothing before, so a
/// typo must not silently fall back to sending markers.
///
/// Kept separate from the other binaries' parsers: two tiny parsers are cheaper
/// than a shared module for a one-flag command (revisit if `main` grows flags).
fn parse_args<I>(args: I) -> Result<Option<CollectionConfig>, String>
where
    I: Iterator<Item = String>,
{
    let mut config = CollectionConfig::default();
    let mut help = false;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => help = true,
            "--internal" => config.traffic_generator = TrafficGenerator::Internal,
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    Ok((!help).then_some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<CollectionConfig>, String> {
        parse_args(arguments.iter().map(|value| value.to_string()))
    }

    #[test]
    fn no_arguments_reproduce_the_defaults() {
        let config = parse(&[]).unwrap().expect("expected a run");
        assert_eq!(config, CollectionConfig::default());
        assert_eq!(config.traffic_generator, TrafficGenerator::External);
        assert!(config.markers_enabled());
    }

    #[test]
    fn internal_switches_the_traffic_source() {
        let config = parse(&["--internal"]).unwrap().expect("expected a run");
        assert_eq!(config.traffic_generator, TrafficGenerator::Internal);
        assert!(!config.markers_enabled());
        // No --output flag exists here, so the suffix applies to the default.
        assert_eq!(
            config.resolved_output(),
            std::path::PathBuf::from("capture_v8_internal.bin")
        );
    }

    #[test]
    fn help_wins_and_bad_arguments_are_rejected() {
        assert!(
            parse(&["--help"]).unwrap().is_none(),
            "--help must not start a capture"
        );
        assert!(parse(&["-h"]).unwrap().is_none());
        // This binary accepted nothing before, so an unknown flag is an error
        // rather than a silent fallback to sending markers.
        assert!(parse(&["--pps", "20"]).is_err());
        assert!(parse(&["capture.bin"]).is_err());
    }
}
