//! Standalone real-time CSI streaming dashboard.
//!
//! Collects raw CSI to a `.bin` file and serves a live amplitude heatmap at the
//! same time. The first heatmap appears after a 3-second warm-up, the data is
//! refreshed every 500 ms, and the time axis keeps at most 30 seconds.
//!
//! ```text
//! cargo run --bin streaming -- --ip 10.178.22.9 --output capture_v8.bin --pps 100 --port 8080
//! ```

use std::{env, process::ExitCode};

use rt_wireless_sensing::collection::TrafficGenerator;
use rt_wireless_sensing::streaming::{StreamingConfig, StreamingError, run};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
collect CSI and serve a live amplitude heatmap

usage: streaming [OPTIONS]

options:
  --ip ADDRESS        ESP32 IPv4 address (default: 10.178.22.9)
  --api-port PORT     ESPectre HTTP API port (default: 62587)
  --marker-port PORT  UDP marker port (default: 5555)
  --pps N             marker packets per second in external mode (default: 20);
                      in internal mode this is only the fallback nominal rate
  --internal          do not send traffic markers: the device generates its own
                      traffic. Requires the device to be in a generator mode
                      (e.g. ping); use `device --show` to check, `device --mode`
                      to change it. Default output gains an `_internal` suffix.
  --origin URL        HTTP Origin header (default: https://test.espectre.dev)
  --output FILE       raw capture file (default: capture_v8.bin)
  --port PORT         dashboard HTTP port on 127.0.0.1 (default: 8080)
  --verbose           print per-chunk collection progress
  --diagnostics       attach the diagnostic trace to every dashboard refresh
                      (~1 MB per poll at 100 slots; off by default)
  -h, --help          show this help

The dashboard is available at http://127.0.0.1:<port> as soon as the command
starts. Press Ctrl+C to stop collection and the server.

traffic mode:
  Without --internal this binary sends the UDP markers (external mode) and
  needs the device in `traffic_generator_mode: external`.
  With --internal nothing is sent and the device must be generating its own
  traffic. A device in the wrong mode stops the run with exit code 3 before the
  dashboard opens. The capture never changes the device's mode.
";

/// Exit code used when the device is in the wrong traffic mode (D3c).
///
/// Distinct from `1` (a runtime failure) and `2` (a usage error), so a script
/// can tell "the device needs switching" from "something broke".
const EXIT_WRONG_MODE: u8 = 3;

/// Parsed command line.
#[derive(Debug)]
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run the streaming dashboard.
    Run(StreamingConfig),
}

#[tokio::main]
async fn main() -> ExitCode {
    let config = match parse_args(env::args().skip(1)) {
        Ok(ParsedArgs::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(ParsedArgs::Run(config)) => config,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match run(&config).await {
        Ok(summary) => {
            println!();
            println!("capture      : {}", summary.output.display());
            println!("bytes        : {}", summary.bytes_written);
            println!("records      : {}", summary.records_decoded);
            println!("window       : {}", summary.window_records);
            println!("duration     : {:.3} s", summary.duration.as_secs_f64());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            exit_code_for(&error)
        }
    }
}

/// Map a streaming failure to its process exit code.
///
/// A wrong device mode is the one failure the user can fix by switching the
/// device, so it gets its own code: a script can then tell it apart from a
/// broken run without parsing stderr. Everything else stays `1`.
fn exit_code_for(error: &StreamingError) -> ExitCode {
    match error {
        StreamingError::WrongDeviceMode(_) => ExitCode::from(EXIT_WRONG_MODE),
        _ => ExitCode::FAILURE,
    }
}

/// Parse the options in [`USAGE`].
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut config = StreamingConfig::default();
    // Tracked separately so the rejection message does not depend on flag order.
    let mut marker_port_given = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--verbose" => config.collection.verbose = true,
            "--diagnostics" => config.diagnostics = true,
            "--internal" => config.collection.traffic_generator = TrafficGenerator::Internal,
            "--ip" => config.collection.esp32_ip = value(&mut args, "--ip")?,
            "--origin" => config.collection.origin = value(&mut args, "--origin")?,
            "--output" => {
                config.collection.output = value(&mut args, "--output")?.into();
                // An operator-chosen name is honoured verbatim.
                config.collection.output_is_default = false;
            }
            "--api-port" => {
                config.collection.api_port = parse_port(&mut args, "--api-port")?;
            }
            "--marker-port" => {
                config.collection.marker_port = parse_port(&mut args, "--marker-port")?;
                marker_port_given = true;
            }
            "--port" => {
                config.port = parse_port(&mut args, "--port")?;
            }
            "--pps" => {
                config.collection.pps = value(&mut args, "--pps")?
                    .parse()
                    .map_err(|_| "--pps requires a positive integer".to_string())?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    // `--marker-port` configures a sender that internal mode never creates, so
    // accepting it silently would leave the operator believing a port choice
    // had taken effect.
    if marker_port_given && config.collection.traffic_generator == TrafficGenerator::Internal {
        return Err(
            "--marker-port is only used when this binary sends the markers; drop it with \
             --internal, or drop --internal to send them"
                .to_string(),
        );
    }

    Ok(ParsedArgs::Run(config))
}

/// Consume and parse a `u16` option value.
fn parse_port<I>(args: &mut I, option: &str) -> Result<u16, String>
where
    I: Iterator<Item = String>,
{
    value(args, option)?
        .parse()
        .map_err(|_| format!("{option} requires a u16 port number"))
}

/// Consume the value that follows an option.
fn value<I>(args: &mut I, option: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<ParsedArgs, String> {
        parse_args(arguments.iter().map(|value| value.to_string()))
    }

    #[test]
    fn defaults_match_the_library_defaults() {
        let ParsedArgs::Run(config) = parse(&[]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config, StreamingConfig::default());
        assert!(!config.collection.verbose);
    }

    #[test]
    fn options_override_the_defaults() {
        let ParsedArgs::Run(config) = parse(&[
            "--ip",
            "192.168.1.50",
            "--output",
            "run.bin",
            "--pps",
            "100",
            "--port",
            "9090",
            "--verbose",
        ])
        .unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config.collection.esp32_ip, "192.168.1.50");
        assert_eq!(
            config.collection.output,
            std::path::PathBuf::from("run.bin")
        );
        assert_eq!(config.collection.pps, 100);
        assert_eq!(config.port, 9090);
        assert!(config.collection.verbose);
    }

    #[test]
    fn rejects_bad_options() {
        assert!(matches!(parse(&["--help"]), Ok(ParsedArgs::Help)));
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--pps"]).is_err());
        assert!(parse(&["--pps", "many"]).is_err());
        assert!(parse(&["--port", "99999"]).is_err());
    }

    // --- P4: --internal -----------------------------------------------------

    #[test]
    fn internal_selects_the_device_generator() {
        let ParsedArgs::Run(config) = parse(&["--internal"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(
            config.collection.traffic_generator,
            TrafficGenerator::Internal
        );
        assert!(!config.collection.markers_enabled());
        assert_eq!(
            config.collection.resolved_output(),
            std::path::PathBuf::from("capture_v8_internal.bin")
        );
    }

    #[test]
    fn internal_with_an_explicit_output_is_not_renamed() {
        let ParsedArgs::Run(config) = parse(&["--internal", "--output", "chosen.bin"]).unwrap()
        else {
            panic!("expected a run");
        };
        assert!(!config.collection.output_is_default);
        assert_eq!(
            config.collection.resolved_output(),
            std::path::PathBuf::from("chosen.bin")
        );
    }

    #[test]
    fn marker_port_is_rejected_with_internal_in_any_order() {
        for args in [
            ["--internal", "--marker-port", "5556"],
            ["--marker-port", "5556", "--internal"],
        ] {
            let error = parse(&args).expect_err("--marker-port means nothing internally");
            assert!(
                error.contains("--marker-port") && error.contains("--internal"),
                "the message must name both flags, got: {error}"
            );
        }

        // Still fine where it is actually used.
        assert!(parse(&["--marker-port", "5556"]).is_ok());
    }

    #[test]
    fn wrong_mode_maps_to_exit_code_three() {
        use rt_wireless_sensing::device::Mismatch;

        let mismatch = StreamingError::WrongDeviceMode(Box::new(Mismatch {
            actual: "ping".to_string(),
            requested: TrafficGenerator::External,
        }));
        assert_eq!(
            exit_code_for(&mismatch),
            ExitCode::from(EXIT_WRONG_MODE),
            "a wrong-mode device is the one failure a script must be able to act on"
        );

        // Every other failure keeps the generic code.
        let other = StreamingError::InvalidConfig("bad".to_string());
        assert_eq!(exit_code_for(&other), ExitCode::FAILURE);
    }
}
