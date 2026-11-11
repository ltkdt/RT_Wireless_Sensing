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

use rt_wireless_sensing::streaming::{StreamingConfig, run};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
collect CSI and serve a live amplitude heatmap

usage: streaming [OPTIONS]

options:
  --ip ADDRESS        ESP32 IPv4 address (default: 10.178.22.9)
  --api-port PORT     ESPectre HTTP API port (default: 62587)
  --marker-port PORT  UDP marker port (default: 5555)
  --pps N             marker packets per second (default: 20)
  --origin URL        HTTP Origin header (default: https://test.espectre.dev)
  --output FILE       raw capture file (default: capture_v8.bin)
  --port PORT         dashboard HTTP port on 127.0.0.1 (default: 8080)
  --verbose           print per-chunk collection progress
  -h, --help          show this help

The dashboard is available at http://127.0.0.1:<port> as soon as the command
starts. Press Ctrl+C to stop collection and the server.
";

/// Parsed command line.
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
            ExitCode::FAILURE
        }
    }
}

/// Parse the options in [`USAGE`].
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut config = StreamingConfig::default();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--verbose" => config.collection.verbose = true,
            "--ip" => config.collection.esp32_ip = value(&mut args, "--ip")?,
            "--origin" => config.collection.origin = value(&mut args, "--origin")?,
            "--output" => {
                config.collection.output = value(&mut args, "--output")?.into();
            }
            "--api-port" => {
                config.collection.api_port = parse_port(&mut args, "--api-port")?;
            }
            "--marker-port" => {
                config.collection.marker_port = parse_port(&mut args, "--marker-port")?;
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
}
