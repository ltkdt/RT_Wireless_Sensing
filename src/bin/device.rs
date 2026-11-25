//! Read or change the ESP32's traffic generator mode.
//!
//! This is the **only** binary that can change the device's mode. The capture
//! binaries (`collect`, `streaming`, `main`) read `GET /sensing` and refuse to
//! record when the device is in the wrong mode, but they never write: changing
//! a setting that survives reboot is an operator decision, not a side effect of
//! measuring.
//!
//! ```text
//! cargo run --bin device -- --ip 10.178.22.9 --show
//! cargo run --bin device -- --ip 10.178.22.9 --mode ping
//! ```

use std::{env, process::ExitCode};

use rt_wireless_sensing::{
    collection::{DEFAULT_API_PORT, DEFAULT_ESP32_IP, DEFAULT_ORIGIN},
    device::{self, DeviceEndpoint, TRAFFIC_GENERATOR_MODES, TrafficGenerator},
};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
read or change the ESP32 traffic generator mode

usage: device (--show | --mode MODE) [OPTIONS]

actions:
  --show              print the current mode and rate, changing nothing
  --mode MODE         set the traffic generator mode, then read it back.
                      MODE is one of: ping, dns, dns_tcp, wifi_raw, external
                      (ping is the device's own generator and its default;
                      external means another host sends the traffic)

options:
  --ip ADDRESS        ESP32 IPv4 address (default: 10.178.22.9)
  --api-port PORT     ESPectre HTTP API port (default: 62587)
  --origin URL        HTTP Origin header (default: https://test.espectre.dev)
  --quiet             print only the mode line
  -h, --help          show this help

The capture binaries read this mode and never write it. Use this command when
a capture stops with \"wrong device mode\": it prints the exact command to run.
";

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Read and print, changing nothing.
    Show,
    /// Write this mode, then read it back.
    Set(String),
}

/// Result of parsing the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run the requested action against this endpoint.
    Run {
        /// Device to talk to.
        endpoint: DeviceEndpoint,
        /// What to do.
        action: Action,
        /// Suppress everything but the mode line.
        quiet: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let (endpoint, action, quiet) = match parse_args(env::args().skip(1)) {
        Ok(ParsedArgs::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(ParsedArgs::Run {
            endpoint,
            action,
            quiet,
        }) => (endpoint, action, quiet),
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match action {
        Action::Show => match device::read(&endpoint).await {
            Some(status) => {
                print_status(&endpoint, &status, quiet);
                ExitCode::SUCCESS
            }
            None => {
                eprintln!(
                    "error: could not read {}/sensing; is the device reachable?",
                    endpoint.base_url()
                );
                ExitCode::FAILURE
            }
        },
        Action::Set(mode) => match device::set_traffic_generator_mode(&endpoint, &mode).await {
            Ok(status) => {
                // A successful PATCH is an acknowledgement, not proof; the
                // confirmed value is what the re-read returned. `Ok` always
                // carries a status, but the firmware may omit the field, so
                // the comparison stays explicit.
                let confirmed = status.traffic_generator_mode.as_deref();
                if confirmed == Some(mode.as_str()) {
                    if !quiet {
                        println!("changed      : {} -> {}", endpoint.base_url(), mode);
                    }
                    print_status(&endpoint, &status, quiet);
                    ExitCode::SUCCESS
                } else {
                    eprintln!(
                        "error: asked for {mode:?} but the device reports {confirmed:?}; \
                         the change did not take effect"
                    );
                    ExitCode::FAILURE
                }
            }
            Err(device::DeviceError::UnknownMode { .. }) => {
                // Unreachable in practice: `parse_args` rejects unknown modes
                // before any request is sent. Kept so the mapping is total.
                eprintln!("error: unknown mode {mode:?}");
                ExitCode::from(2)
            }
            Err(error) => {
                // Includes `Unconfirmed`: the write was accepted but the device
                // could not be re-read, so the outcome is unknown rather than
                // failed. The message says so and points at `--show`.
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Print what the device reported, and what that means for a capture.
fn print_status(endpoint: &DeviceEndpoint, status: &device::DeviceStatus, quiet: bool) {
    let mode = status
        .traffic_generator_mode
        .as_deref()
        .unwrap_or("unknown");

    if quiet {
        println!("{mode}");
        return;
    }

    println!("device       : {}", endpoint.base_url());
    println!("traffic mode : {mode}{}", describe_mode(status));
    match status.csi_target_pps {
        Some(pps) => println!("target rate  : {pps} PPS"),
        None => println!("target rate  : not reported"),
    }
    println!("next         : {}", next_step(status));
}

/// The parenthetical after the mode name, e.g. "(the device generates traffic)".
fn describe_mode(status: &device::DeviceStatus) -> &'static str {
    match status.is_internal() {
        Some(true) => "  (the device generates its own traffic)",
        Some(false) => "  (another host must send the traffic)",
        None => "  (mode not reported by this firmware)",
    }
}

/// One line telling the operator which capture command matches this mode.
fn next_step(status: &device::DeviceStatus) -> &'static str {
    match status.traffic_generator() {
        Some(TrafficGenerator::Internal) => "cargo run --bin collect -- --internal",
        Some(TrafficGenerator::External) => "cargo run --bin collect -- --pps <N>",
        None => "set a mode with --mode before capturing",
    }
}

/// Parse the command line. Action first documented in `USAGE`.
fn parse_args<I>(arguments: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut endpoint = DeviceEndpoint::new(DEFAULT_ESP32_IP, DEFAULT_API_PORT, DEFAULT_ORIGIN);
    let mut show = false;
    let mut mode: Option<String> = None;
    let mut quiet = false;

    let mut args = arguments.peekable();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--show" => show = true,
            "--quiet" => quiet = true,
            "--mode" => {
                let requested = value(&mut args, "--mode")?;
                if !TRAFFIC_GENERATOR_MODES.contains(&requested.as_str()) {
                    return Err(format!(
                        "--mode must be one of: {}, got {requested:?}",
                        TRAFFIC_GENERATOR_MODES.join(", ")
                    ));
                }
                mode = Some(requested);
            }
            "--ip" => endpoint.host = value(&mut args, "--ip")?,
            "--origin" => endpoint.origin = value(&mut args, "--origin")?,
            "--api-port" => {
                endpoint.api_port = value(&mut args, "--api-port")?
                    .parse()
                    .map_err(|_| "--api-port requires a u16 port".to_string())?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    // Exactly one action: the two are different operations, and guessing would
    // risk writing when the user only meant to look.
    let action = match (show, mode) {
        (true, None) => Action::Show,
        (false, Some(mode)) => Action::Set(mode),
        (true, Some(_)) => {
            return Err("--show and --mode are mutually exclusive".to_string());
        }
        (false, None) => {
            return Err("one of --show or --mode is required".to_string());
        }
    };

    Ok(ParsedArgs::Run {
        endpoint,
        action,
        quiet,
    })
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

    /// The endpoint plus action of a successful run.
    fn run(arguments: &[&str]) -> (DeviceEndpoint, Action, bool) {
        match parse(arguments).expect("arguments should parse") {
            ParsedArgs::Run {
                endpoint,
                action,
                quiet,
            } => (endpoint, action, quiet),
            ParsedArgs::Help => panic!("expected a run, got help"),
        }
    }

    #[test]
    fn show_uses_the_project_defaults() {
        let (endpoint, action, quiet) = run(&["--show"]);
        assert_eq!(
            endpoint,
            DeviceEndpoint::new(DEFAULT_ESP32_IP, DEFAULT_API_PORT, DEFAULT_ORIGIN)
        );
        assert_eq!(action, Action::Show);
        assert!(!quiet);
    }

    #[test]
    fn mode_builds_a_set_action() {
        let (_, action, _) = run(&["--mode", "ping"]);
        assert_eq!(action, Action::Set("ping".to_string()));
    }

    #[test]
    fn show_and_mode_are_mutually_exclusive() {
        // Neither is an error: the user must say which operation they mean, so
        // `--show --mode ping` cannot silently write.
        assert!(parse(&[]).is_err());
        assert!(parse(&["--show", "--mode", "ping"]).is_err());
        assert!(parse(&["--mode", "ping", "--show"]).is_err());
    }

    #[test]
    fn the_missing_action_message_names_both_options() {
        let message = parse(&[]).unwrap_err();
        assert!(message.contains("--show"), "message: {message}");
        assert!(message.contains("--mode"), "message: {message}");
    }

    #[test]
    fn mode_rejects_unknown_values() {
        // `pacing` is a real ESPectre runtime concept but not a valid
        // `traffic_generator_mode`; it must be rejected before any request.
        for bad in ["pacing", "PING", "", "external "] {
            let message = parse(&["--mode", bad]).unwrap_err();
            assert!(
                message.contains("--mode must be one of"),
                "mode {bad:?} gave: {message}"
            );
        }
    }

    #[test]
    fn every_documented_mode_parses() {
        for mode in TRAFFIC_GENERATOR_MODES {
            let (_, action, _) = run(&["--mode", mode]);
            assert_eq!(action, Action::Set(mode.to_string()));
        }
    }

    #[test]
    fn options_override_the_defaults() {
        let (endpoint, action, quiet) = run(&[
            "--ip",
            "192.168.1.50",
            "--api-port",
            "8080",
            "--origin",
            "https://example.test",
            "--mode",
            "dns_tcp",
            "--quiet",
        ]);
        assert_eq!(endpoint.host, "192.168.1.50");
        assert_eq!(endpoint.api_port, 8080);
        assert_eq!(endpoint.origin, "https://example.test");
        assert_eq!(endpoint.base_url(), "http://192.168.1.50:8080/espectre/v1");
        assert_eq!(action, Action::Set("dns_tcp".to_string()));
        assert!(quiet);
    }

    #[test]
    fn options_apply_to_show_too() {
        let (endpoint, action, _) = run(&["--show", "--ip", "10.0.0.9", "--api-port", "1234"]);
        assert_eq!(endpoint.host, "10.0.0.9");
        assert_eq!(endpoint.api_port, 1234);
        assert_eq!(action, Action::Show);
    }

    #[test]
    fn help_is_recognized_in_both_forms() {
        assert_eq!(parse(&["--help"]).unwrap(), ParsedArgs::Help);
        assert_eq!(parse(&["-h"]).unwrap(), ParsedArgs::Help);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(parse(&["--api-port", "not-a-port", "--show"]).is_err());
        assert!(parse(&["--api-port"]).is_err());
        assert!(parse(&["--mode"]).is_err());
        assert!(parse(&["--nonsense"]).is_err());
        assert!(parse(&["stray"]).is_err());
    }

    #[test]
    fn status_lines_describe_each_mode() {
        let internal = device::DeviceStatus {
            traffic_generator_mode: Some("ping".to_string()),
            csi_target_pps: Some(100),
        };
        assert!(describe_mode(&internal).contains("device generates"));
        assert!(next_step(&internal).contains("--internal"));

        let external = device::DeviceStatus {
            traffic_generator_mode: Some("external".to_string()),
            csi_target_pps: Some(20),
        };
        assert!(describe_mode(&external).contains("another host"));
        assert!(next_step(&external).contains("--pps"));

        let unknown = device::DeviceStatus::default();
        assert!(describe_mode(&unknown).contains("not reported"));
        assert!(next_step(&unknown).contains("--mode"));
    }
}
