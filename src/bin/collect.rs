//! Standalone CSI collector.
//!
//! This binary exposes the same collection routine the default `main` uses,
//! with command-line settings for the device and output file.
//!
//! ```text
//! cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin
//! ```

use std::{env, process::ExitCode};

use rt_wireless_sensing::collection::{CollectionConfig, collect};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
collect a raw ESPectre V8 CSI stream

usage: collect [OPTIONS]

options:
  --ip ADDRESS        ESP32 IPv4 address (default: 10.178.22.9)
  --api-port PORT     ESPectre HTTP API port (default: 62587)
  --marker-port PORT  UDP marker port (default: 5555)
  --pps N             marker packets per second (default: 20)
  --origin URL        HTTP Origin header (default: https://test.espectre.dev)
  --output FILE       capture file to overwrite (default: capture_v8.bin)
  --quiet             do not print per-chunk progress
  -h, --help          show this help
";

/// Result of parsing the command line.
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
            println!("Sensing: {}", summary.sensing);
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
            ExitCode::FAILURE
        }
    }
}

/// Parse the options in [`USAGE`].
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut config = CollectionConfig::default();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--quiet" => config.verbose = false,
            "--ip" => config.esp32_ip = value(&mut args, "--ip")?,
            "--origin" => config.origin = value(&mut args, "--origin")?,
            "--output" => config.output = value(&mut args, "--output")?.into(),
            "--api-port" => {
                config.api_port = value(&mut args, "--api-port")?
                    .parse()
                    .map_err(|_| "--api-port requires a u16 port".to_string())?;
            }
            "--marker-port" => {
                config.marker_port = value(&mut args, "--marker-port")?
                    .parse()
                    .map_err(|_| "--marker-port requires a u16 port".to_string())?;
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
}
