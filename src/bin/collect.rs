//! Standalone CSI collector.
//!
//! This binary exposes the same collection routine the default `main` uses,
//! with command-line settings for the device and output file.
//!
//! ```text
//! cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin
//! ```

use std::{env, process::ExitCode, time::Duration};

use rt_wireless_sensing::collection::{
    CollectionConfig, CollectionError, TrafficGenerator, collect,
};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
collect a raw ESPectre V8 CSI stream

usage: collect [OPTIONS]

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
  --output FILE       capture file to overwrite (default: capture_v8.bin)
  --duration DURATION stop recording after DURATION, e.g. 20, 20s, 500ms, 2m
                      (default unit: seconds; omit for no limit)
  --quiet             do not print per-chunk progress
  -h, --help          show this help

traffic mode:
  Without --internal this binary sends the UDP markers (external mode) and
  needs the device in `traffic_generator_mode: external`.
  With --internal nothing is sent and the device must be generating its own
  traffic. A device in the wrong mode stops the run with exit code 3 and no
  capture file. The capture never changes the device's mode.
";

/// Exit code used when the device is in the wrong traffic mode (D3c).
///
/// Distinct from `1` (a runtime failure) and `2` (a usage error), so a script
/// can tell "the device needs switching" from "something broke".
const EXIT_WRONG_MODE: u8 = 3;

/// Result of parsing the command line.
#[derive(Debug)]
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run collection with these settings.
    Run(CollectionConfig),
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

    match collect(&config).await {
        Ok(summary) => {
            println!("Capabilities: {}", summary.capabilities);
            println!("device       : {}", summary.device_line());
            println!("rate         : {}", summary.rate_line());
            println!(
                "capture      : {} ({} bytes)",
                summary.output.display(),
                summary.bytes_written
            );
            println!(
                "duration     : {:.3} s at {} PPS",
                summary.duration.as_secs_f64(),
                summary.configured_pps
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            exit_code_for(&error)
        }
    }
}

/// Map a collection failure to its process exit code.
///
/// A wrong device mode is the one failure the user can fix by switching the
/// device, so it gets its own code: a script can then tell it apart from a
/// broken run without parsing stderr. Everything else stays `1`.
fn exit_code_for(error: &CollectionError) -> ExitCode {
    match error {
        CollectionError::WrongDeviceMode(_) => ExitCode::from(EXIT_WRONG_MODE),
        _ => ExitCode::FAILURE,
    }
}

/// Parse the options in [`USAGE`].
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut config = CollectionConfig::default();
    // Tracked separately from `config` so the rejection message can be produced
    // once the whole command line is known, whatever order the flags came in.
    let mut marker_port_given = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--quiet" => config.verbose = false,
            "--internal" => config.traffic_generator = TrafficGenerator::Internal,
            "--ip" => config.esp32_ip = value(&mut args, "--ip")?,
            "--origin" => config.origin = value(&mut args, "--origin")?,
            "--output" => {
                config.output = value(&mut args, "--output")?.into();
                // An operator-chosen name is honoured verbatim.
                config.output_is_default = false;
            }
            "--duration" => {
                config.max_duration = Some(parse_duration(&value(&mut args, "--duration")?)?);
            }
            "--api-port" => {
                config.api_port = value(&mut args, "--api-port")?
                    .parse()
                    .map_err(|_| "--api-port requires a u16 port".to_string())?;
            }
            "--marker-port" => {
                config.marker_port = value(&mut args, "--marker-port")?
                    .parse()
                    .map_err(|_| "--marker-port requires a u16 port".to_string())?;
                marker_port_given = true;
            }
            "--pps" => {
                config.pps = value(&mut args, "--pps")?
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
    // accepting it silently would make a leftover flag from an external run
    // look like it meant something.
    if marker_port_given && config.traffic_generator == TrafficGenerator::Internal {
        return Err(
            "--marker-port is only used when this binary sends the markers; drop it with \
             --internal, or drop --internal to send them"
                .to_string(),
        );
    }

    Ok(ParsedArgs::Run(config))
}

/// Consume the value that follows an option.
fn value<I>(args: &mut I, option: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

/// Parse a duration such as `20`, `20s`, `500ms`, `2m`, or `1h`.
///
/// A bare number is interpreted in seconds.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, multiplier) = if let Some(n) = text.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = text.strip_suffix('h') {
        (n, 3600.0)
    } else {
        (text, 1.0)
    };
    let seconds = number
        .parse::<f64>()
        .map_err(|_| format!("--duration requires a number, got {text:?}"))?
        * multiplier;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(format!("--duration must be positive, got {text:?}"));
    }
    Ok(Duration::from_secs_f64(seconds))
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
        assert_eq!(config, CollectionConfig::default());
    }

    #[test]
    fn options_override_the_defaults() {
        let ParsedArgs::Run(config) = parse(&[
            "--ip",
            "192.168.1.50",
            "--api-port",
            "8080",
            "--pps",
            "10",
            "--output",
            "test.bin",
            "--quiet",
        ])
        .unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config.esp32_ip, "192.168.1.50");
        assert_eq!(config.api_port, 8080);
        assert_eq!(config.pps, 10);
        assert_eq!(config.output, std::path::PathBuf::from("test.bin"));
        assert!(!config.verbose);
    }

    #[test]
    fn help_and_bad_options_are_reported() {
        assert!(matches!(parse(&["--help"]), Ok(ParsedArgs::Help)));
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--pps"]).is_err());
        assert!(parse(&["--pps", "many"]).is_err());
    }

    // --- P4: --internal -----------------------------------------------------

    #[test]
    fn internal_selects_the_device_generator() {
        let ParsedArgs::Run(config) = parse(&["--internal"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config.traffic_generator, TrafficGenerator::Internal);
        assert!(!config.markers_enabled());
        // Nothing is sent, so the marker rate is no longer meaningful — the
        // library only insists on `pps > 0` in marker mode, which is why the
        // default 20 is harmless here rather than load-bearing.
        assert_eq!(
            config.marker_port,
            rt_wireless_sensing::collection::DEFAULT_MARKER_PORT
        );
    }

    #[test]
    fn internal_renames_only_the_default_output() {
        let ParsedArgs::Run(default) = parse(&["--internal"]).unwrap() else {
            panic!("expected a run");
        };
        assert!(default.output_is_default);
        assert_eq!(
            default.resolved_output(),
            std::path::PathBuf::from("capture_v8_internal.bin")
        );

        // An explicit --output is honoured verbatim in either mode.
        let ParsedArgs::Run(explicit) = parse(&["--internal", "--output", "chosen.bin"]).unwrap()
        else {
            panic!("expected a run");
        };
        assert!(!explicit.output_is_default);
        assert_eq!(
            explicit.resolved_output(),
            std::path::PathBuf::from("chosen.bin")
        );
    }

    #[test]
    fn output_clears_the_default_flag_in_external_mode_too() {
        let ParsedArgs::Run(config) = parse(&["--output", "run.bin"]).unwrap() else {
            panic!("expected a run");
        };
        assert!(!config.output_is_default);
        assert_eq!(
            config.resolved_output(),
            std::path::PathBuf::from("run.bin")
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

        let mismatch = CollectionError::WrongDeviceMode(Box::new(Mismatch {
            actual: "external".to_string(),
            requested: TrafficGenerator::Internal,
        }));
        assert_eq!(
            exit_code_for(&mismatch),
            ExitCode::from(EXIT_WRONG_MODE),
            "a wrong-mode device is the one failure a script must be able to act on"
        );

        // Every other failure keeps the generic code.
        let other = CollectionError::InvalidPps;
        assert_eq!(exit_code_for(&other), ExitCode::FAILURE);
    }

    #[test]
    fn duration_is_unlimited_by_default() {
        let ParsedArgs::Run(config) = parse(&[]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config.max_duration, None);
    }

    #[test]
    fn duration_flag_uses_seconds_by_default() {
        let ParsedArgs::Run(config) = parse(&["--duration", "20"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(config.max_duration, Some(Duration::from_secs(20)));
    }

    #[test]
    fn duration_flag_accepts_unit_suffixes() {
        let cases = [
            ("500ms", Duration::from_millis(500)),
            ("1.5s", Duration::from_millis(1500)),
            ("2m", Duration::from_secs(120)),
            ("1h", Duration::from_secs(3600)),
        ];
        for (input, expected) in cases {
            let ParsedArgs::Run(config) = parse(&["--duration", input]).unwrap() else {
                panic!("expected a run for {input}");
            };
            assert_eq!(config.max_duration, Some(expected), "input: {input}");
        }
    }

    #[test]
    fn duration_flag_rejects_bad_values() {
        assert!(parse(&["--duration"]).is_err());
        assert!(parse(&["--duration", "abc"]).is_err());
        assert!(parse(&["--duration", "0"]).is_err());
        assert!(parse(&["--duration", "-5"]).is_err());
    }
}
