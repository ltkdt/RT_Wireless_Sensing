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

use std::{env, path::PathBuf, process::ExitCode, time::Duration};

use rt_wireless_sensing::collection::{NominalPpsSource, traffic_mode_from_filename};
use rt_wireless_sensing::dsp::{InterpMethod, NormScheme};
use rt_wireless_sensing::visualization::{
    DspOptions, VisualizationData, VisualizationOptions, serve_dashboard,
};

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
  --pps PPS          nominal packet rate used for the diagnostic slot grid
                     (default: measured effective rate, which shrinks the grid
                     exactly for the seconds that miss samples)
  --duration DUR     slot span of the run the operator asked for, written as
                     5, 5s, 500ms or 1m (default: the recorded span)
                     the capture file is never truncated: records past this
                     span stay in the payload and are reported as extra
  --no-dsp           skip the precomputed DSP view (resampled amplitude/phase)
                     to keep the payload small; the DSP toggle stays hidden
  --dsp-interp M     resampling interpolation: nearest | linear | cubic
                     (default: cubic)
  --dsp-norm S       normalization on top of the resampling: none |
                     per-subcarrier | per-sample | min-max (default: none)
  --dsp-max-gap-ms MS  mask grid columns whose source gap exceeds MS
                     milliseconds (default: no threshold, every gap bridged)
  --serve            start an embedded asynchronous web server
  --port PORT        port for the web server (default: 8080)
  --open             open the generated HTML or web server in default browser
  -q, --quiet        suppress summary text output
  -h, --help         show this help
";

/// Parsed command-line options.
#[derive(Debug, PartialEq)]
struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    json_output: Option<PathBuf>,
    pps: Option<f64>,
    duration: Option<Duration>,
    dsp: Option<DspOptions>,
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

    let bytes = match std::fs::read(&options.input) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!(
                "error: failed to read '{}': {err}",
                options.input.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let source_name = options.input.file_name().and_then(|name| name.to_str());
    // DSP parameters are fixed here, once: the result is embedded in the
    // payload, so the standalone HTML and `--serve` show identical data.
    let data = match VisualizationData::from_bytes_with_options(
        &bytes,
        source_name,
        VisualizationOptions {
            nominal_pps: options.pps,
            nominal_pps_source: offline_nominal_source(options.pps),
            traffic_mode: traffic_mode_from_filename(source_name.unwrap_or_default()),
            capture_end: true,
            diagnostics: true,
            dsp: options.dsp,
            // The capture keeps every record the run produced, and a fixed
            // length run usually overshoots its deadline by a chunk, so the
            // recorded span is not the span that was asked for. `--duration`
            // declares it; without the flag the recorded span is used.
            span_us: options.duration.map(|duration| duration.as_micros() as u64),
        },
    ) {
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
    let mut pps = None;
    let mut duration = None;
    let mut dsp = Some(DspOptions::default());
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
            "--pps" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--pps requires a number".to_string())?;
                let parsed = val
                    .parse::<f64>()
                    .map_err(|_| format!("invalid pps: {val}"))?;
                if !parsed.is_finite() || parsed <= 0.0 {
                    return Err(format!("invalid pps: {val}"));
                }
                pps = Some(parsed);
            }
            "--duration" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--duration requires a value".to_string())?;
                duration = Some(parse_duration(&val)?);
            }
            "--no-dsp" => dsp = None,
            "--dsp-interp" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--dsp-interp requires a method".to_string())?;
                let interp = match val.as_str() {
                    "nearest" | "nearest-neighbor" | "nearest_neighbour" => InterpMethod::Nearest,
                    "linear" => InterpMethod::Linear,
                    "cubic" | "cubic_spline" | "cubic-spline" => InterpMethod::CubicSpline,
                    other => return Err(format!("invalid --dsp-interp: {other}")),
                };
                dsp = Some(DspOptions {
                    interp,
                    ..dsp.unwrap_or_default()
                });
            }
            "--dsp-norm" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--dsp-norm requires a scheme".to_string())?;
                let norm = match val.as_str() {
                    "none" => None,
                    "per-subcarrier" | "per_subcarrier" => Some(NormScheme::PerSubcarrier),
                    "per-sample" | "per_sample" => Some(NormScheme::PerSample),
                    "min-max" | "min_max" | "minmax" => Some(NormScheme::MinMax),
                    other => return Err(format!("invalid --dsp-norm: {other}")),
                };
                dsp = Some(DspOptions {
                    norm,
                    ..dsp.unwrap_or_default()
                });
            }
            "--dsp-max-gap-ms" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--dsp-max-gap-ms requires a number".to_string())?;
                let parsed = val
                    .parse::<f64>()
                    .map_err(|_| format!("invalid --dsp-max-gap-ms: {val}"))?;
                if !parsed.is_finite() || parsed < 0.0 {
                    return Err(format!("invalid --dsp-max-gap-ms: {val}"));
                }
                dsp = Some(DspOptions {
                    max_gap_ms: Some(parsed),
                    ..dsp.unwrap_or_default()
                });
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
        pps,
        duration,
        dsp,
        serve,
        port,
        open,
        quiet,
    }))
}

/// Parse a duration such as `5`, `5s`, `500ms`, `2m`, or `1h`.
///
/// A bare number is interpreted in seconds; the grammar is the one `collect`
/// accepts for its own `--duration`.
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
    if let Some(declared) = m.declared_span_seconds {
        println!("declared span: {declared:.3} s (the run that was asked for)");
    }
    println!("rate         : {:.2} frames/s", m.effective_pps);
    match m.nominal_pps {
        Some(pps) => println!("nominal pps  : {:.0}", pps),
        None => println!("nominal pps  : <not requested>"),
    }
    if let Some(ch) = m.channel {
        println!("channel      : {}", ch);
    }
    // Coverage is a slot count, not a record count: the slots the run was
    // declared to cover, how many of them received a record, and the records
    // that landed in a slot another record already filled.
    if let Some(diag) = &data.diagnostics {
        let summary = &diag.summary;
        println!(
            "slots        : {} filled / {} total ({:.1}% empty)",
            summary.filled_slots,
            summary.total_slots,
            summary.loss_ratio * 100.0
        );
        println!(
            "extra records: {} (a slot took more than one record)",
            summary.extra_records
        );
        if diag.records_past_span > 0 {
            println!(
                "past span    : {} record(s) arrived after the declared span",
                diag.records_past_span
            );
        }
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

/// Provenance of the nominal rate for an **offline** payload.
///
/// `visualize` has no device to ask, so the only nominal rate it can know about
/// is one the operator typed: `--pps` → [`NominalPpsSource::Argument`]. Without
/// the flag the nominal stays `None` (the dashboard shows the measured rate)
/// rather than inventing a device rate no live session produced.
///
/// The traffic mode is *not* derived here — see [`traffic_mode_from_filename`],
/// which reads it back from the `_internal` suffix. Renaming a capture can
/// therefore lie about the mode; that is accepted, because the alternative is
/// no offline provenance at all.
fn offline_nominal_source(pps: Option<f64>) -> Option<NominalPpsSource> {
    pps.map(|_| NominalPpsSource::Argument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_wireless_sensing::collection::TrafficGenerator;
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
    fn duration_sets_the_slot_span() {
        let ParsedArgs::Run(options) = parse(&["run.bin", "--duration", "5"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.duration, Some(Duration::from_secs(5)));

        let ParsedArgs::Run(options) = parse(&["run.bin", "--duration", "500ms"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.duration, Some(Duration::from_millis(500)));

        let ParsedArgs::Run(options) = parse(&["run.bin"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.duration, None);
    }

    #[test]
    fn rejects_bad_durations() {
        assert!(parse(&["--duration"]).is_err());
        assert!(parse(&["--duration", "zero"]).is_err());
        assert!(parse(&["--duration", "0"]).is_err());
        assert!(parse(&["--duration", "-5"]).is_err());
    }

    #[test]
    fn duration_bounds_the_slot_grid() {
        // 5 s at 100 PPS is 500 slots, whatever the recording span turned out
        // to be: the deadline stops the reader, not the writer.
        let bytes = std::fs::read(rt_wireless_sensing::fixtures::capture_v8()).expect("fixture");
        let data = VisualizationData::from_bytes_with_options(
            &bytes,
            Some("capture_v8_internal.bin"),
            VisualizationOptions {
                nominal_pps: Some(100.0),
                nominal_pps_source: Some(NominalPpsSource::Argument),
                traffic_mode: None,
                capture_end: true,
                diagnostics: true,
                dsp: None,
                span_us: Some(5_000_000),
            },
        )
        .expect("payload");
        let summary = &data.diagnostics.expect("trace").summary;
        assert_eq!(summary.total_slots, 500);
        assert_eq!(summary.filled_slots + summary.empty_slots, 500);
    }

    #[test]
    fn rejects_unknown_options() {
        assert!(parse(&["--invalid"]).is_err());
        assert!(parse(&["--output"]).is_err());
        assert!(parse(&["--port", "not_a_num"]).is_err());
    }

    #[test]
    fn dsp_view_is_on_by_default() {
        let ParsedArgs::Run(options) = parse(&["run.bin"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.dsp, Some(DspOptions::default()));
    }

    #[test]
    fn no_dsp_turns_the_block_off() {
        let ParsedArgs::Run(options) = parse(&["run.bin", "--no-dsp"]).unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.dsp, None);
    }

    #[test]
    fn dsp_flags_are_parsed_independently() {
        let ParsedArgs::Run(options) = parse(&[
            "run.bin",
            "--dsp-interp",
            "linear",
            "--dsp-norm",
            "min-max",
            "--dsp-max-gap-ms",
            "250",
        ])
        .unwrap() else {
            panic!("expected a run");
        };
        let dsp = options.dsp.unwrap();
        assert_eq!(dsp.interp, InterpMethod::Linear);
        assert_eq!(dsp.norm, Some(NormScheme::MinMax));
        assert_eq!(dsp.max_gap_ms, Some(250.0));

        // Omitting a flag keeps its default, it does not clear the others.
        let ParsedArgs::Run(options) = parse(&["run.bin", "--dsp-norm", "per-sample"]).unwrap()
        else {
            panic!("expected a run");
        };
        let dsp = options.dsp.unwrap();
        assert_eq!(dsp.interp, InterpMethod::CubicSpline);
        assert_eq!(dsp.norm, Some(NormScheme::PerSample));
        assert_eq!(dsp.max_gap_ms, None);
    }

    #[test]
    fn dsp_accepts_spelling_variants() {
        for (arg, expected) in [
            ("cubic", InterpMethod::CubicSpline),
            ("cubic_spline", InterpMethod::CubicSpline),
            ("nearest", InterpMethod::Nearest),
            ("linear", InterpMethod::Linear),
        ] {
            let ParsedArgs::Run(options) = parse(&["run.bin", "--dsp-interp", arg]).unwrap() else {
                panic!("expected a run");
            };
            assert_eq!(options.dsp.unwrap().interp, expected);
        }
        let ParsedArgs::Run(options) =
            parse(&["run.bin", "--dsp-norm", "none"]).unwrap()
        else {
            panic!("expected a run");
        };
        assert_eq!(options.dsp.unwrap().norm, None);
    }

    #[test]
    fn rejects_bad_dsp_values() {
        assert!(parse(&["run.bin", "--dsp-interp"]).is_err());
        assert!(parse(&["run.bin", "--dsp-interp", "spline"]).is_err());
        assert!(parse(&["run.bin", "--dsp-norm", "zscore"]).is_err());
        assert!(parse(&["run.bin", "--dsp-max-gap-ms", "-5"]).is_err());
        assert!(parse(&["run.bin", "--dsp-max-gap-ms", "soon"]).is_err());
    }

    // --- P4: the `_internal` suffix restores the traffic mode ---------------

    #[test]
    fn internal_suffix_restores_the_traffic_mode() {
        // `visualize` has no device, so the file name is the only provenance.
        for name in [
            "capture_v8_internal.bin",
            "/tmp/capture_v8_internal.bin",
            "run_internal.bin",
        ] {
            assert_eq!(
                traffic_mode_from_filename(name),
                Some(TrafficGenerator::Internal),
                "{name} should read back as internal"
            );
        }

        // Everything else is unknown, not "external": a name proves nothing.
        for name in ["capture_v8.bin", "internal.bin", "capture_internal_v8.bin"] {
            assert_eq!(traffic_mode_from_filename(name), None, "{name}");
        }
    }

    #[test]
    fn offline_provenance_only_claims_what_the_argument_says() {
        // `--pps` is the one rate an offline run genuinely knows.
        assert_eq!(
            offline_nominal_source(Some(100.0)),
            Some(NominalPpsSource::Argument)
        );
        // Without it the nominal stays unset rather than being invented from
        // the measured rate.
        assert_eq!(offline_nominal_source(None), None);
    }
}
