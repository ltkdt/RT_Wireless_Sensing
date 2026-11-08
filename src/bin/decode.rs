//! Command-line decoder for an ESPectre V8 CSI capture.
//!
//! Usage:
//!
//! ```text
//! cargo run --bin decode -- capture_v8.bin
//! cargo run --bin decode -- capture_v8.bin --limit 10
//! cargo run --bin decode -- capture_v8.bin --json
//! ```
//!
//! The tool decodes every frame, prints one line (or one JSON object) per
//! record, and ends with a summary that reports sequence gaps and counter
//! deltas. It never prints the raw I/Q arrays: those belong in a binary or
//! numeric export, not in a metadata report.

use std::{env, fs, path::Path, path::PathBuf, process::ExitCode};

use rt_wireless_sensing::decoder::{CsiPacket, FrameStreamDecoder};
use serde_json::json;

/// Printed when the user asks for help or passes an unknown option.
const USAGE: &str = "\
decode an ESPectre V8 CSI capture

usage: decode [FILE] [OPTIONS]

arguments:
  FILE            raw capture written from /espectre/v1/csi
                  (default: capture_v8.bin)

options:
  --json          print JSON Lines instead of a text table
  --limit N       print at most N records (all records are still decoded)
  --quiet         print only the summary line
  -h, --help      show this help
";

/// FNV-1a 64-bit offset basis, used for the payload checksum.
const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Parsed command-line options.
struct Options {
    path: PathBuf,
    json: bool,
    limit: Option<usize>,
    quiet: bool,
}

/// Result of parsing the command line.
enum ParsedArgs {
    /// Print usage and exit successfully.
    Help,
    /// Run the decoder.
    Run(Options),
}

fn main() -> ExitCode {
    let parsed = match parse_args(env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let options = match parsed {
        ParsedArgs::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ParsedArgs::Run(options) => options,
    };

    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `--options` and the optional file path.
fn parse_args<I>(mut args: I) -> Result<ParsedArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut path = None;
    let mut json = false;
    let mut limit = None;
    let mut quiet = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--limit" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--limit requires a number".to_string())?;
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --limit value: {value}"))?;
                limit = Some(parsed);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                if path.is_some() {
                    return Err("only one capture file may be given".to_string());
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    Ok(ParsedArgs::Run(Options {
        path: path.unwrap_or_else(|| PathBuf::from("capture_v8.bin")),
        json,
        limit,
        quiet,
    }))
}

/// Decode the capture and print the report.
fn run(options: &Options) -> Result<(), String> {
    let bytes = fs::read(&options.path)
        .map_err(|error| format!("cannot read {}: {error}", options.path.display()))?;

    let mut decoder = FrameStreamDecoder::new();
    decoder.push(&bytes);

    let mut index = 0usize;
    let mut first: Option<CsiPacket> = None;
    let mut last: Option<CsiPacket> = None;
    let mut previous_stream_sequence: Option<u64> = None;
    let mut gaps = 0u64;
    let mut missing = 0u64;
    let mut first_drop = 0u64;
    let mut first_backpressure = 0u64;
    let mut printed = 0usize;
    // Whole-capture checksum of the raw I/Q bytes. ESPectre's Python parser can
    // compute the same value, which makes payload parity easy to verify.
    let mut payload_hash = FNV1A64_OFFSET_BASIS;

    // Decode until the buffer no longer holds a complete frame.
    loop {
        let packet = match decoder.next_frame() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => {
                return Err(format!(
                    "decode failed after {index} frame(s), at byte {}: {error}",
                    decoder.bytes_consumed()
                ));
            }
        };

        // Track stream-level accounting. The decoder already rejects a
        // non-increasing sequence, so a >1 step is a real device-side drop.
        if let Some(previous) = previous_stream_sequence {
            if packet.frame.stream_sequence > previous + 1 {
                gaps += 1;
                missing += packet.frame.stream_sequence - previous - 1;
            }
        } else {
            first_drop = packet.frame.raw_drop_total;
            first_backpressure = packet.frame.raw_send_backpressure_total;
        }
        previous_stream_sequence = Some(packet.frame.stream_sequence);

        // Remember the ends of the capture for the summary.
        if first.is_none() {
            first = Some(packet.clone());
        }

        let should_print = !options.quiet && options.limit.is_none_or(|limit| printed < limit);
        if should_print {
            if options.json {
                print_json_record(index, &packet);
            } else {
                print_text_record(index, &packet);
            }
            printed += 1;
        }

        // Hash every raw sample exactly as received (two's-complement byte).
        for &sample in &packet.iq_raw {
            payload_hash ^= u64::from(sample as u8);
            payload_hash = payload_hash.wrapping_mul(FNV1A64_PRIME);
        }

        last = Some(packet);
        index += 1;
    }

    // Anything still buffered is a truncated final frame or stray bytes.
    decoder
        .finish()
        .map_err(|error| format!("input ended with undecoded data: {error}"))?;

    if options.json {
        print_json_summary(
            index,
            first.as_ref(),
            last.as_ref(),
            gaps,
            missing,
            first_drop,
            first_backpressure,
            payload_hash,
        );
    } else {
        print_text_summary(
            &options.path,
            index,
            decoder.bytes_consumed(),
            first.as_ref(),
            last.as_ref(),
            gaps,
            missing,
            first_drop,
            first_backpressure,
            payload_hash,
        );
    }

    Ok(())
}

/// Print one record as a compact text line.
fn print_text_record(index: usize, packet: &CsiPacket) {
    let csi = &packet.csi;
    let noise = csi
        .valid_noise_floor_dbm()
        .map_or_else(|| "n/a".to_string(), |value| value.to_string());
    let rx_ts = packet
        .valid_wifi_rx_ts_us()
        .map_or_else(|| "n/a".to_string(), |value| value.to_string());

    println!(
        "#{index:>4} seq={:<6} stream={:<6} dev=0x{:016x} ch={:<3} rssi={:>4}dBm noise={:<4} \
         chip={:<4} phy={:<7} ltf={:<8} bw={:>3}MHz sc={:<3} bytes={:<3} flags=0x{:02x} \
         fresh={} ticks={}us rx_ts={}us",
        csi.seq_num,
        packet.frame.stream_sequence,
        csi.device_id,
        csi.channel,
        csi.rssi_dbm,
        noise,
        csi.chip_name(),
        csi.phy_mode_name(),
        csi.ltf_type_name(),
        csi.channel_width_name(),
        csi.num_sc,
        csi.csi_len_bytes,
        csi.flags,
        csi.is_fresh(),
        csi.device_ticks_us,
        rx_ts,
    );
}

/// Print one record as a JSON object on its own line.
fn print_json_record(index: usize, packet: &CsiPacket) {
    let csi = &packet.csi;
    let record = json!({
        "type": "record",
        "index": index,
        "stream_sequence": packet.frame.stream_sequence,
        "seq_num": csi.seq_num,
        "session_id": hex(&packet.frame.session_id),
        "device_id": csi.device_id,
        "device_ticks_us": csi.device_ticks_us,
        "wifi_rx_ts_us": packet.valid_wifi_rx_ts_us(),
        "wifi_rx_start_ts_ns": packet.valid_wifi_rx_start_ts_ns(),
        "channel": csi.channel,
        "rssi_dbm": csi.rssi_dbm,
        "noise_floor_dbm": csi.valid_noise_floor_dbm(),
        "chip": csi.chip_name(),
        "phy_mode": csi.phy_mode_name(),
        "ltf_type": csi.ltf_type_name(),
        "channel_width": csi.channel_width_name(),
        "num_subcarriers": csi.num_sc,
        "csi_len_bytes": csi.csi_len_bytes,
        "flags": csi.flags,
        "first_word_invalid": csi.first_word_invalid(),
        "fresh": csi.is_fresh(),
        "fresh_record_total": packet.frame.fresh_record_total,
        "raw_drop_total": packet.frame.raw_drop_total,
        "raw_send_backpressure_total": packet.frame.raw_send_backpressure_total,
    });
    println!("{record}");
}

/// Print the text summary.
#[allow(clippy::too_many_arguments)]
fn print_text_summary(
    path: &Path,
    frames: usize,
    bytes_consumed: u64,
    first: Option<&CsiPacket>,
    last: Option<&CsiPacket>,
    gaps: u64,
    missing: u64,
    first_drop: u64,
    first_backpressure: u64,
    payload_hash: u64,
) {
    println!();
    println!("capture      : {}", path.display());
    println!("frames       : {frames}");
    println!("bytes        : {bytes_consumed}");
    println!("payload hash : 0x{payload_hash:016x} (FNV-1a 64)");

    if let Some(first) = first {
        println!("session id   : {}", hex(&first.frame.session_id));
        println!("device id    : 0x{:016x}", first.csi.device_id);
    }
    if let (Some(first), Some(last)) = (first, last) {
        println!(
            "sequence     : {}..{} (gaps: {gaps}, missing: {missing})",
            first.frame.stream_sequence, last.frame.stream_sequence
        );
        println!(
            "drops        : {first_drop} -> {}",
            last.frame.raw_drop_total
        );
        println!(
            "backpressure : {first_backpressure} -> {}",
            last.frame.raw_send_backpressure_total
        );

        // Device ticks are the best sample clock we have for a rate estimate.
        if last.csi.device_ticks_us > first.csi.device_ticks_us {
            let duration_s =
                (last.csi.device_ticks_us - first.csi.device_ticks_us) as f64 / 1_000_000.0;
            let pps = if frames > 1 && duration_s > 0.0 {
                (frames - 1) as f64 / duration_s
            } else {
                0.0
            };
            println!("duration     : {duration_s:.3} s (device ticks)");
            println!("rate         : {pps:.2} records/s");
        }
    } else {
        println!("sequence     : (empty capture)");
    }
}

/// Print the summary as one JSON object.
#[allow(clippy::too_many_arguments)]
fn print_json_summary(
    frames: usize,
    first: Option<&CsiPacket>,
    last: Option<&CsiPacket>,
    gaps: u64,
    missing: u64,
    first_drop: u64,
    first_backpressure: u64,
    payload_hash: u64,
) {
    let session = first.map(|packet| hex(&packet.frame.session_id));
    let device_id = first.map(|packet| packet.csi.device_id);
    let first_sequence = first.map(|packet| packet.frame.stream_sequence);
    let last_sequence = last.map(|packet| packet.frame.stream_sequence);
    let last_drop = last.map_or(first_drop, |packet| packet.frame.raw_drop_total);
    let last_backpressure = last.map_or(first_backpressure, |packet| {
        packet.frame.raw_send_backpressure_total
    });

    let summary = json!({
        "type": "summary",
        "frames": frames,
        "session_id": session,
        "device_id": device_id,
        "first_stream_sequence": first_sequence,
        "last_stream_sequence": last_sequence,
        "sequence_gaps": gaps,
        "missing_records": missing,
        "raw_drop_total": last_drop,
        "raw_send_backpressure_total": last_backpressure,
        "payload_fnv1a64": format!("0x{payload_hash:016x}"),
    });
    println!("{summary}");
}

/// Render bytes as lowercase hex without separators.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
