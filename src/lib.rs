//! RT-Wireless-Sensing library.
//!
//! The crate is split into three independent modules that mirror the data
//! pipeline:
//!
//! * [`collection`] — talk to the ESPectre device and write the raw CSI byte
//!   stream to a `.bin` capture.
//! * [`decoder`] — parse and validate the framed V8 records, incrementally and
//!   without resynchronizing on corrupt input.
//! * [`saving`] — decode a `.bin` capture and export timestamp, amplitude, and
//!   phase arrays to a `.npz` archive.
//! * [`visualization`] — prepare decoded CSI records and render an interactive
//!   2D amplitude heatmap web dashboard.
//!
//! [`csi_header`] holds the record schema used by the decoder.
//!
//! The layering is intentional: the 60-byte HTTP frame prefix and the 64-byte
//! CSI record header are different structures and must not be confused.

pub mod collection;
pub mod csi_header;
pub mod decoder;
pub mod saving;
pub mod visualization;
