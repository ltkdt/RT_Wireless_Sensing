//! Standalone CSI Amplitude Heatmap Visualizer CLI.
//!
//! Decodes a raw ESPectre V8 capture and generates an interactive, high-performance
//! 2D amplitude heatmap dashboard (HTML5 Canvas 2D).
//!
//! ```text
//! cargo run --bin visualize -- capture_v8.bin
//! cargo run --bin visualize -- capture_v8.bin --output dashboard.html --open
//! cargo run --bin visualize -- capture_v8.bin --serve --port 8080
//! ```

use std::{env, path::PathBuf, process::ExitCode};

use rt_wireless_sensing::visualization::{VisualizationData, serve_dashboard};

/// Printed for `--help` and bad arguments.
const USAGE: &str = "\
decode a raw ESPectre V8 capture and generate a 2D CSI amplitude heatmap

usage: visualize [FILE] [OPTIONS]

arguments:
  FILE             raw capture written by `collect`
                   (default: capture_v8.bin)

options:
  -o, --output FILE  write standalone HTML dashboard to FILE
                     (default: <stem>_heatmap.html)
  --json FILE        export JSON visualization dataset to FILE
  --serve            start an embedded asynchronous web server
  --port PORT        port for the web server (default: 8080)
  --open             open the generated HTML or web server in default browser
  -q, --quiet        suppress summary text output
  -h, --help         show this help
";

/// Parsed command-line options.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    json_output: Option<PathBuf>,
    serve: bool,
    port: u16,
    open: bool,
    quiet: bool,
}

/// Result of parsing the command line.
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run the visualizer.
    Run(Options),
}

#[tokio::main]
async fn main() -> ExitCode {
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

    let data = match VisualizationData::from_file(&options.input) {
        Ok(data) => data,
        Err(err) => {
            eprintln!(
                "error: failed to load and decode '{}': {err}",
                options.input.display()
            );
            return ExitCode::FAILURE;
        }
    };

    if !options.quiet {
        print_summary(&data);
    }

    // Export JSON if requested
    if let Some(json_path) = &options.json_output {
        if let Err(err) = data.export_json(json_path) {
            eprintln!(
                "error: failed to export JSON to '{}': {err}",
                json_path.display()
            );
            return ExitCode::FAILURE;
        }
        if !options.quiet {
            println!("exported JSON: {}", json_path.display());
        }
    }

    // Determine HTML output path
    let html_path = options.output.unwrap_or_else(|| {
        let stem = options
            .input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("csi");
        PathBuf::from(format!("{stem}_heatmap.html"))
    });

    // Always export the standalone HTML bundle
    if let Err(err) = data.export_html(&html_path) {
        eprintln!(
            "error: failed to export HTML dashboard to '{}': {err}",
            html_path.display()
        );
        return ExitCode::FAILURE;
    }
    if !options.quiet {
        let size = std::fs::metadata(&html_path).map(|m| m.len()).unwrap_or(0);
        println!("exported HTML: {} ({} bytes)", html_path.display(), size);
    }

    // Open browser if requested
    if options.open && !options.serve {
        let abs_path = std::fs::canonicalize(&html_path).unwrap_or(html_path.clone());
        let _ = open_in_browser(&abs_path.to_string_lossy());
    }

    // Start web server if --serve was requested
    if options.serve {
        let bind_addr = format!("127.0.0.1:{}", options.port);
        let url = format!("http://{bind_addr}");
        if options.open {
            let _ = open_in_browser(&url);
        }
        if let Err(err) = serve_dashboard(data, &bind_addr).await {
            eprintln!("error: server failure: {err}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}

fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut input = None;
    let mut output = None;
    let mut json_output = None;
    let mut serve = false;
    let mut port = 8080;
    let mut open = false;
    let mut quiet = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "-o" | "--output" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--output requires a path".to_string())?;
                output = Some(PathBuf::from(val));
            }
            "--json" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--json requires a path".to_string())?;
                json_output = Some(PathBuf::from(val));
            }
            "--serve" => serve = true,
            "--port" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--port requires a number".to_string())?;
                port = val
                    .parse::<u16>()
                    .map_err(|_| format!("invalid port: {val}"))?;
            }
            "--open" => open = true,
            "-q" | "--quiet" => quiet = true,
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
        json_output,
        serve,
        port,
        open,
        quiet,
    }))
}

fn print_summary(data: &VisualizationData) {
    let m = &data.metadata;
    println!(
        "source       : {}",
        m.source.as_deref().unwrap_or("<in-memory>")
    );
    println!("records      : {}", m.total_frames);
    println!("subcarriers  : {}", m.subcarriers);
    println!("start clock  : {} µs", m.start_time_us);
    println!("duration     : {:.3} s", m.duration_seconds);
    println!("rate         : {:.2} frames/s", m.effective_pps);
    if let Some(ch) = m.channel {
        println!("channel      : {}", ch);
    }
}

fn open_in_browser(target: &str) -> Result<(), std::io::Error> {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "start";

    std::process::Command::new(cmd)
        .arg(target)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(arguments: &[&str]) -> Result<ParsedArgs, String> {
        parse_args(arguments.iter().map(|value| value.to_string()))
    }

    #[test]
    fn defaults_to_standard_capture() {
        let ParsedArgs::Run(options) = parse(&[]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.input, Path::new("capture_v8.bin"));
        assert_eq!(options.output, None);
        assert!(!options.serve);
        assert_eq!(options.port, 8080);
    }

    #[test]
    fn accepts_flags() {
        let ParsedArgs::Run(options) = parse(&[
            "run.bin", "--output", "out.html", "--serve", "--port", "9090", "--open", "--quiet",
        ])
        .unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.input, Path::new("run.bin"));
        assert_eq!(options.output, Some(PathBuf::from("out.html")));
        assert!(options.serve);
        assert_eq!(options.port, 9090);
        assert!(options.open);
        assert!(options.quiet);
    }

    #[test]
    fn rejects_unknown_options() {
        assert!(parse(&["--invalid"]).is_err());
        assert!(parse(&["--output"]).is_err());
        assert!(parse(&["--port", "not_a_num"]).is_err());
    }
}
