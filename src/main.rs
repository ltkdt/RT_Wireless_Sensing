//! Default binary: collect the ESPectre CSI stream.
//!
//! `main` is intentionally a thin wrapper around
//! [`rt_wireless_sensing::collection::collect`]. The default configuration
//! reproduces the original streamer: ESP32 at `10.178.22.9`, 20 PPS markers,
//! and `capture_v8.bin` in the current directory.
//!
//! For command-line options, decoding, or array export use the companion
//! binaries:
//!
//! ```text
//! cargo run --bin collect -- --help
//! cargo run --bin decode -- capture_v8.bin
//! cargo run --bin save -- capture_v8.bin
//! ```

use rt_wireless_sensing::collection::{self, CollectionConfig};

#[tokio::main]
async fn main() -> Result<(), collection::CollectionError> {
    let config = CollectionConfig::default();
    let summary = collection::collect(&config).await?;

    println!("Capabilities: {}", summary.capabilities);
    println!("Sensing: {}", summary.sensing);
    println!(
        "Saved {} bytes to {} in {:.3} s at {} PPS",
        summary.bytes_written,
        summary.output.display(),
        summary.duration.as_secs_f64(),
        summary.configured_pps
    );

    Ok(())
}
