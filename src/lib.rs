//! RT-Wireless-Sensing library.
//!
//! This crate turns the raw bytes captured from an ESPectre device into
//! structured CSI records. The binary in `src/main.rs` only records the
//! stream; decoding and any later processing use the modules below.
//!
//! * [`csi_header`] — schema and constants for the 64-byte V8 CSI record.
//! * [`decoder`] — little-endian parsing, envelope/record validation, and the
//!   incremental [`decoder::FrameStreamDecoder`].
//!
//! The layering is intentional: the 60-byte HTTP frame prefix and the 64-byte
//! CSI record header are different structures and must not be confused.

pub mod csi_header;
pub mod decoder;
