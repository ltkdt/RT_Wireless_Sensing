//! RT-Wireless-Sensing library.
//!
//! The crate is split into three independent modules that mirror the data
//! pipeline:
//!
//! * [`collection`] — talk to the ESPectre device and write the raw CSI byte
//!   stream to a `.bin` capture.
//! * [`device`] — read and control the device's traffic generator mode. The
//!   only module that speaks to the device about its own state, and the only
//!   place a mode is written.
//! * [`decoder`] — parse and validate the framed V8 records, incrementally and
//!   without resynchronizing on corrupt input.
//! * [`saving`] — decode a `.bin` capture and export timestamp, amplitude, and
//!   phase arrays to a `.npz` archive.
//! * [`dsp`] — synchronous signal processing over decoded CSI arrays
//!   ([`dsp::CsiSignal`]): normalization, resampling, outlier removal,
//!   biquad filters, FFT/STFT.
//! * [`visualization`] — prepare decoded CSI records and render an interactive
//!   2D amplitude heatmap web dashboard.
//! * [`streaming`] — collect, decode live, and serve a sliding-window
//!   heatmap dashboard at the same time.
//! * [`fixtures`] — the one place that knows where the `samples/` capture
//!   fixtures live, so tests do not each rebuild the path.
//!
//! [`csi_header`] holds the record schema used by the decoder.
//!
//! The layering is intentional: the 60-byte HTTP frame prefix and the 64-byte
//! CSI record header are different structures and must not be confused.

pub mod collection;
pub mod csi_header;
pub mod decoder;
pub mod device;
pub mod dsp;
pub mod fixtures;
pub mod saving;
pub mod streaming;
pub mod visualization;
