//! Standalone capture-to-`.npz` exporter.
//!
//! Decodes a raw ESPectre V8 capture and writes one `.npz` archive with 129
//! arrays (timestamp + 64 amplitudes + 64 phases for the standard capture).
//!
//! ```text
//! cargo run --bin save -- capture_v8.bin
//! cargo run --bin save -- capture_v8.bin --output arrays.npz
//! ```

use std::{env, path::PathBuf, process::ExitCode};

use rt_wireless_sensing::saving::{SaveSummary, save_capture, save_capture_to};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
decode a raw ESPectre V8 capture and write timestamp/amplitude/phase arrays

usage: save [FILE] [OPTIONS]

arguments:
  FILE            raw capture written by `collect`
                  (default: capture_v8.bin)

options:
  --output FILE   write the .npz here instead of next to the input
  -h, --help      show this help
";

/// Parsed command-line options.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
}

/// Result of parsing the command line.
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run the exporter.
    Run(Options),
}

fn main() -> ExitCode {
    let options = match parse_args(env::args().skip(1)) {
        Ok(ParsedArgs::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(ParsedArgs::Run(options)) => options,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let result = match &options.output {
        Some(output) => save_capture_to(&options.input, output),
        None => save_capture(&options.input),
    };

    match result {
        Ok(summary) => {
            print_summary(&summary);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `[FILE] [--output FILE]`.
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut input = None;
    let mut output = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--output" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--output requires a path".to_string())?;
                output = Some(PathBuf::from(value));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                if input.is_some() {
                    return Err("only one capture file may be given".to_string());
                }
                input = Some(PathBuf::from(other));
            }
        }
    }

    Ok(ParsedArgs::Run(Options {
        input: input.unwrap_or_else(|| PathBuf::from("capture_v8.bin")),
        output,
    }))
}

/// Print a compact description of the written archive.
fn print_summary(summary: &SaveSummary) {
    println!("input        : {}", summary.input.display());
    println!(
        "output       : {} ({} bytes)",
        summary.output.display(),
        summary.bytes_written
    );
    println!("records      : {}", summary.records);
    println!("subcarriers  : {}", summary.subcarriers);
    println!("arrays       : {}", summary.arrays);
    println!("duration     : {:.3} ms", summary.duration_ms);
    println!("rate         : {:.2} records/s", summary.pps);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_wireless_sensing::saving::npz_path_for;
    use std::path::Path;

    fn parse(arguments: &[&str]) -> Result<ParsedArgs, String> {
        parse_args(arguments.iter().map(|value| value.to_string()))
    }

    #[test]
    fn defaults_to_the_standard_capture() {
        let ParsedArgs::Run(options) = parse(&[]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.input, Path::new("capture_v8.bin"));
        assert_eq!(options.output, None);
    }

    #[test]
    fn derives_the_output_from_the_input() {
        let ParsedArgs::Run(options) = parse(&["data/capture.bin"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(
            npz_path_for(&options.input),
            PathBuf::from("data/capture.npz")
        );
    }

    #[test]
    fn accepts_an_explicit_output() {
        let ParsedArgs::Run(options) =
            parse(&["capture_v8.bin", "--output", "arrays.npz"]).unwrap()
        else {
            panic!("expected a run");
        };
        assert_eq!(options.output, Some(PathBuf::from("arrays.npz")));
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--output"]).is_err());
        assert!(parse(&["a.bin", "b.bin"]).is_err());
    }
}
