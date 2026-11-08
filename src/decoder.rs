//! Decoder for the ESPectre V8 raw CSI stream.
//!
//! ESPectre's `/espectre/v1/csi` endpoint writes a sequence of framed records.
//! One frame on the wire is:
//!
//! ```text
//! offset 0                     60                        60 + header_len
//! ┌────────────────────────────┬──────────────────────────┬─────────────────┐
//! │ HTTP frame prefix (60 B)   │ CSI record header        │ CSI I/Q payload │
//! │ RawCsiHttpFramePrefix      │ RawCsiRecordHeaderV8     │ csi_len_bytes   │
//! └────────────────────────────┴──────────────────────────┴─────────────────┘
//! ```
//!
//! The 60-byte prefix is transport/session framing, **not** a CSI header. The
//! length rules are:
//!
//! * `prefix.header_len` is the size of the prefix itself (60).
//! * `prefix.record_len` is the size of the record header plus payload.
//! * therefore `frame_len = prefix.header_len + prefix.record_len`.
//!
//! The parser below mirrors ESPectre's reference implementation:
//!
//! * `_consume_raw_frames()` in `tools/lib/csi_io.py` for the envelope and the
//!   cross-checks between prefix and record counters.
//! * `parse_csi_record()` for the record header and payload.
//!
//! It uses explicit little-endian reads instead of casting the byte slice into
//! a Rust struct, because Rust's layout and padding are not a reliable
//! substitute for the wire format.
//!
//! Two entry points are exposed:
//!
//! * [`FrameStreamDecoder`] for incremental input (files, sockets, split
//!   reads). It buffers partial frames and yields one [`CsiPacket`] at a time.
//! * [`HttpFrameHeader::parse`] plus the internal record parser for callers
//!   that already have one complete frame.

use std::fmt;

use crate::csi_header::{
    CSI_HEADER_LEN, CsiHeader, MAX_PAYLOAD_BYTES, RAW_CSI_RECORD_MAGIC, RAW_CSI_RECORD_VERSION_V8,
};

/// Size of the HTTP frame prefix in bytes (`RawCsiHttpFramePrefix`).
pub const HTTP_FRAME_PREFIX_LEN: usize = 60;

/// Size of the HTTP frame prefix as it appears in the wire `header_len` field.
pub const HTTP_FRAME_PREFIX_LEN_U16: u16 = HTTP_FRAME_PREFIX_LEN as u16;

/// Protocol version of the stream framing (`prefix.version`).
pub const RAW_CSI_PROTOCOL_VERSION: u8 = 1;

/// `prefix.magic`: the bytes `"ESPR"` read as a little-endian `u32`.
pub const RAW_CSI_RESPONSE_MAGIC: u32 = 0x5250_5345;

/// Length of `prefix.session_id`.
pub const SESSION_ID_LEN: usize = 16;

/// Largest `prefix.record_len` the format allows.
///
/// `record_len` covers the record header plus the payload; the record header is
/// at least 64 bytes and the payload is capped at 512 bytes.
pub const MAX_RECORD_BYTES: u16 = CSI_HEADER_LEN as u16 + MAX_PAYLOAD_BYTES;

/// The 60-byte transport prefix that precedes every CSI record.
///
/// This is deliberately a separate type from [`CsiHeader`]: mixing the two
/// layers up is the easiest way to mis-parse the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFrameHeader {
    /// Must be [`RAW_CSI_RESPONSE_MAGIC`].
    pub magic: u32,
    /// Stream framing version; must be [`RAW_CSI_PROTOCOL_VERSION`].
    pub version: u8,
    /// Version of the record that follows; must be
    /// [`RAW_CSI_RECORD_VERSION_V8`].
    pub record_version: u8,
    /// Size of this prefix; must be [`HTTP_FRAME_PREFIX_LEN_U16`].
    pub header_len: u16,
    /// Random session identifier, constant for the whole connection.
    pub session_id: [u8; SESSION_ID_LEN],
    /// One-based sequence of packets **offered** to the stream, including the
    /// ones the device dropped, so a gap shows how many were lost.
    pub stream_sequence: u64,
    /// Size of the record header and payload that follow, in bytes.
    pub record_len: u16,
    /// Reserved; must be zero. (This is not the CSI record's flag byte.)
    pub flags: u16,
    /// Records sent in this session, including this one.
    pub fresh_record_total: u64,
    /// Records not sent because the queue was full, the packet was invalid, or
    /// its batch failed to send.
    pub raw_drop_total: u64,
    /// Batch sends the client did not accept.
    pub raw_send_backpressure_total: u64,
}

impl HttpFrameHeader {
    /// Number of bytes this prefix occupies on the wire.
    pub const WIRE_LEN: usize = HTTP_FRAME_PREFIX_LEN;

    /// Parse one 60-byte prefix from the start of `bytes`.
    ///
    /// `base` is the absolute stream/file offset of `bytes[0]`; it is only used
    /// to report errors at meaningful positions.
    ///
    /// This function checks only that the bytes exist. Magic, version, session,
    /// counter and length rules are enforced by
    /// [`FrameStreamDecoder::next_frame`], because they need per-stream state.
    pub fn parse(bytes: &[u8], base: u64) -> Result<Self, DecodeError> {
        let mut r = LeReader::new(bytes, base);

        // Field order must match `<IBBH16sQHHQQQ` exactly.
        let magic = r.u32("HTTP frame magic")?;
        let version = r.u8("HTTP frame version")?;
        let record_version = r.u8("HTTP frame record version")?;
        let header_len = r.u16("HTTP frame header length")?;

        // A fixed-size array is simpler to compare than a slice.
        let mut session_id = [0u8; SESSION_ID_LEN];
        session_id.copy_from_slice(r.take(SESSION_ID_LEN, "HTTP frame session id")?);

        let stream_sequence = r.u64("HTTP frame stream sequence")?;
        let record_len = r.u16("HTTP frame record length")?;
        let flags = r.u16("HTTP frame flags")?;
        let fresh_record_total = r.u64("HTTP frame fresh record total")?;
        let raw_drop_total = r.u64("HTTP frame raw drop total")?;
        let raw_send_backpressure_total = r.u64("HTTP frame raw backpressure total")?;

        Ok(Self {
            magic,
            version,
            record_version,
            header_len,
            session_id,
            stream_sequence,
            record_len,
            flags,
            fresh_record_total,
            raw_drop_total,
            raw_send_backpressure_total,
        })
    }

    /// Total frame size declared by this prefix: prefix + record.
    pub fn frame_len(&self) -> usize {
        self.header_len as usize + self.record_len as usize
    }
}

/// One decoded V8 CSI record: transport metadata, CSI metadata, raw I/Q.
///
/// The raw payload is preserved exactly as it arrived. Derived values
/// (complex, amplitude, phase) are computed on demand so that decoding never
/// alters the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsiPacket {
    /// Envelope for this frame.
    pub frame: HttpFrameHeader,
    /// CSI record header.
    pub csi: CsiHeader,
    /// Interleaved signed 8-bit I/Q samples, in wire order:
    /// `[Q0, I0, Q1, I1, ...]`. Never reordered or normalized during decoding.
    pub iq_raw: Vec<i8>,
}

impl CsiPacket {
    /// Number of complex subcarriers in the payload.
    pub fn subcarriers(&self) -> usize {
        self.csi.num_sc as usize
    }

    /// Raw interleaved I/Q bytes, in wire order.
    pub fn iq(&self) -> &[i8] {
        &self.iq_raw
    }

    /// Iterate over `(I, Q)` pairs, converting the wire order `[Q, I]`.
    ///
    /// ESPectre's reference parser uses `q = iq_raw[0::2]` and
    /// `i = iq_raw[1::2]`, so the first byte of each pair is Q.
    pub fn iq_pairs(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        // `as_chunks` yields `&[i8; 2]`; record validation already guarantees an
        // even payload length, so the remainder is always empty.
        self.iq_raw.as_chunks::<2>().0.iter().map(|pair| {
            let q = pair[0] as f32;
            let i = pair[1] as f32;
            (i, q)
        })
    }

    /// Amplitude of every subcarrier: `hypot(I, Q)`.
    pub fn amplitudes(&self) -> Vec<f32> {
        self.iq_pairs().map(|(i, q)| i.hypot(q)).collect()
    }

    /// Phase of every subcarrier in radians: `atan2(Q, I)`.
    pub fn phases(&self) -> Vec<f32> {
        self.iq_pairs().map(|(i, q)| q.atan2(i)).collect()
    }

    /// Driver receive timestamp, or `None` when the flag says it is invalid.
    pub fn valid_wifi_rx_ts_us(&self) -> Option<u32> {
        self.csi.valid_wifi_rx_ts_us()
    }

    /// Receive-start timestamp, or `None` when the flag says it is invalid.
    pub fn valid_wifi_rx_start_ts_ns(&self) -> Option<u64> {
        self.csi.valid_wifi_rx_start_ts_ns()
    }

    /// Noise floor, or `None` for the device's `-128` sentinel.
    pub fn valid_noise_floor_dbm(&self) -> Option<i8> {
        self.csi.valid_noise_floor_dbm()
    }
}

/// Everything that can go wrong while decoding a frame.
///
/// Every variant carries the absolute offset of the offending frame or field so
/// a caller can report exactly where the stream stopped instead of silently
/// resynchronizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not enough bytes for a field, header, or payload.
    Truncated {
        /// What was being read, for example `"CSI payload"`.
        context: &'static str,
        /// Absolute offset where the read started.
        offset: u64,
        /// Bytes required.
        needed: usize,
        /// Bytes actually available.
        available: usize,
    },
    /// The envelope magic was not `"ESPR"`.
    BadFrameMagic {
        /// Absolute offset of the frame.
        offset: u64,
        /// Value that was found.
        found: u32,
    },
    /// The envelope protocol version was not 1.
    UnsupportedProtocolVersion {
        /// Absolute offset of the version byte.
        offset: u64,
        /// Value that was found.
        found: u8,
    },
    /// The envelope announced a record version other than 8.
    UnsupportedRecordVersion {
        /// Absolute offset of the record-version byte.
        offset: u64,
        /// Value that was found.
        found: u8,
    },
    /// The envelope header length was not 60.
    BadFrameHeaderLength {
        /// Absolute offset of the length field.
        offset: u64,
        /// Value that was found.
        found: u16,
        /// Value the format requires.
        expected: u16,
    },
    /// The session id changed inside one stream.
    SessionChanged {
        /// Absolute offset of the frame.
        offset: u64,
        /// Session id established by the first frame.
        expected: [u8; SESSION_ID_LEN],
        /// Session id in this frame.
        found: [u8; SESSION_ID_LEN],
    },
    /// `stream_sequence` did not strictly increase.
    StreamSequenceNotIncreasing {
        /// Absolute offset of the sequence field.
        offset: u64,
        /// Previous accepted value (`0` before the first frame).
        previous: u64,
        /// Value in this frame.
        found: u64,
    },
    /// The reserved envelope flags were not zero.
    NonZeroFrameFlags {
        /// Absolute offset of the flags field.
        offset: u64,
        /// Value that was found.
        found: u16,
    },
    /// `record_len` was zero or exceeded [`MAX_RECORD_BYTES`].
    RecordLengthOutOfRange {
        /// Absolute offset of the length field.
        offset: u64,
        /// Value that was found.
        found: u16,
        /// Largest allowed value.
        maximum: u16,
    },
    /// `fresh_record_total` was not the previous value plus one.
    FreshTotalNotSequential {
        /// Absolute offset of the counter.
        offset: u64,
        /// Previous accepted value (`0` before the first frame).
        previous: u64,
        /// Value in this frame.
        found: u64,
    },
    /// The drop counter moved backwards.
    DropCounterWentBackwards {
        /// Absolute offset of the counter.
        offset: u64,
        /// Previous accepted value.
        previous: u64,
        /// Value in this frame.
        found: u64,
    },
    /// The backpressure counter moved backwards.
    BackpressureCounterWentBackwards {
        /// Absolute offset of the counter.
        offset: u64,
        /// Previous accepted value.
        previous: u64,
        /// Value in this frame.
        found: u64,
    },
    /// The record magic was not `"SC"`.
    BadRecordMagic {
        /// Absolute offset of the record.
        offset: u64,
        /// Value that was found.
        found: u16,
    },
    /// The record header declared fewer than 64 bytes.
    RecordHeaderTooShort {
        /// Absolute offset of the record header length.
        offset: u64,
        /// Value that was found.
        found: u8,
        /// Smallest valid header size.
        minimum: u8,
    },
    /// The record declared an empty payload.
    ZeroPayloadLength {
        /// Absolute offset of the payload length.
        offset: u64,
    },
    /// `csi_len_bytes` did not equal `num_sc * 2`.
    PayloadLengthMismatch {
        /// Absolute offset of the payload length.
        offset: u64,
        /// Declared subcarrier count.
        num_subcarriers: u16,
        /// Declared payload length.
        payload_len: u16,
    },
    /// The payload exceeded [`MAX_PAYLOAD_BYTES`].
    PayloadTooLarge {
        /// Absolute offset of the payload length.
        offset: u64,
        /// Value that was found.
        found: u16,
        /// Largest allowed value.
        maximum: u16,
    },
    /// The record size disagreed with the envelope's `record_len`.
    RecordLengthMismatch {
        /// Absolute offset of the record.
        offset: u64,
        /// `record_len` from the envelope.
        frame_record_len: u16,
        /// Header length from the record.
        record_header_len: u8,
        /// Payload length from the record.
        payload_len: u16,
    },
    /// A record counter did not mirror the envelope counter it duplicates.
    CounterMismatch {
        /// Absolute offset of the field.
        offset: u64,
        /// Name of the record field.
        field: &'static str,
        /// Value implied by the envelope.
        expected: u64,
        /// Value found in the record.
        found: u64,
    },
    /// Bytes were left over after the last complete frame.
    TrailingBytes {
        /// Absolute offset of the first leftover byte.
        offset: u64,
        /// Number of leftover bytes.
        count: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated {
                context,
                offset,
                needed,
                available,
            } => write!(
                f,
                "truncated {context} at offset {offset}: need {needed} bytes, have {available}"
            ),
            Self::BadFrameMagic { offset, found } => {
                write!(f, "bad HTTP frame magic at offset {offset}: 0x{found:08x}")
            }
            Self::UnsupportedProtocolVersion { offset, found } => write!(
                f,
                "unsupported HTTP protocol version at offset {offset}: {found}"
            ),
            Self::UnsupportedRecordVersion { offset, found } => write!(
                f,
                "unsupported CSI record version at offset {offset}: {found}"
            ),
            Self::BadFrameHeaderLength {
                offset,
                found,
                expected,
            } => write!(
                f,
                "bad HTTP frame header length at offset {offset}: {found}, expected {expected}"
            ),
            Self::SessionChanged {
                offset,
                expected,
                found,
            } => write!(
                f,
                "session id changed at offset {offset}: expected {expected:02x?}, found {found:02x?}"
            ),
            Self::StreamSequenceNotIncreasing {
                offset,
                previous,
                found,
            } => write!(
                f,
                "stream sequence did not increase at offset {offset}: previous {previous}, found {found}"
            ),
            Self::NonZeroFrameFlags { offset, found } => write!(
                f,
                "nonzero HTTP frame flags at offset {offset}: 0x{found:04x}"
            ),
            Self::RecordLengthOutOfRange {
                offset,
                found,
                maximum,
            } => write!(
                f,
                "record length out of range at offset {offset}: {found}, maximum {maximum}"
            ),
            Self::FreshTotalNotSequential {
                offset,
                previous,
                found,
            } => write!(
                f,
                "fresh record total is not sequential at offset {offset}: previous {previous}, found {found}"
            ),
            Self::DropCounterWentBackwards {
                offset,
                previous,
                found,
            } => write!(
                f,
                "drop counter went backwards at offset {offset}: previous {previous}, found {found}"
            ),
            Self::BackpressureCounterWentBackwards {
                offset,
                previous,
                found,
            } => write!(
                f,
                "backpressure counter went backwards at offset {offset}: previous {previous}, found {found}"
            ),
            Self::BadRecordMagic { offset, found } => {
                write!(f, "bad CSI record magic at offset {offset}: 0x{found:04x}")
            }
            Self::RecordHeaderTooShort {
                offset,
                found,
                minimum,
            } => write!(
                f,
                "CSI record header too short at offset {offset}: {found}, minimum {minimum}"
            ),
            Self::ZeroPayloadLength { offset } => {
                write!(f, "zero CSI payload length at offset {offset}")
            }
            Self::PayloadLengthMismatch {
                offset,
                num_subcarriers,
                payload_len,
            } => write!(
                f,
                "CSI payload length mismatch at offset {offset}: {num_subcarriers} subcarriers need {}, found {payload_len}",
                u32::from(*num_subcarriers) * 2
            ),
            Self::PayloadTooLarge {
                offset,
                found,
                maximum,
            } => write!(
                f,
                "CSI payload too large at offset {offset}: {found}, maximum {maximum}"
            ),
            Self::RecordLengthMismatch {
                offset,
                frame_record_len,
                record_header_len,
                payload_len,
            } => write!(
                f,
                "record length mismatch at offset {offset}: envelope says {frame_record_len}, record needs {}",
                u32::from(*record_header_len) + u32::from(*payload_len)
            ),
            Self::CounterMismatch {
                offset,
                field,
                expected,
                found,
            } => write!(
                f,
                "counter mismatch for {field} at offset {offset}: expected {expected}, found {found}"
            ),
            Self::TrailingBytes { offset, count } => write!(
                f,
                "trailing bytes at offset {offset}: {count} bytes after the last complete frame"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Bounds-checked little-endian reader over a byte slice.
///
/// Every read is explicit about byte order and length. `base` turns a relative
/// position into an absolute offset for error reporting.
struct LeReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    base: u64,
}

impl<'a> LeReader<'a> {
    fn new(bytes: &'a [u8], base: u64) -> Self {
        Self {
            bytes,
            pos: 0,
            base,
        }
    }

    /// Absolute offset of the next unread byte.
    fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    /// Return the next `count` bytes and advance the cursor.
    fn take(&mut self, count: usize, context: &'static str) -> Result<&'a [u8], DecodeError> {
        // `self.pos` never exceeds `bytes.len()`, so this cannot underflow.
        let available = self.bytes.len() - self.pos;
        if available < count {
            return Err(DecodeError::Truncated {
                context,
                offset: self.offset(),
                needed: count,
                available,
            });
        }
        let start = self.pos;
        self.pos += count;
        // Copy the slice reference out of `self` so the returned lifetime is the
        // input lifetime, not the lifetime of the `&mut self` borrow.
        let bytes = self.bytes;
        Ok(&bytes[start..self.pos])
    }

    /// Skip `count` bytes, for example a future record-header extension.
    fn skip(&mut self, count: usize, context: &'static str) -> Result<(), DecodeError> {
        self.take(count, context).map(|_| ())
    }

    fn u8(&mut self, context: &'static str) -> Result<u8, DecodeError> {
        Ok(self.take(1, context)?[0])
    }

    fn i8(&mut self, context: &'static str) -> Result<i8, DecodeError> {
        // Reinterpreting the bit pattern is exactly what the wire format means.
        Ok(self.u8(context)? as i8)
    }

    fn u16(&mut self, context: &'static str) -> Result<u16, DecodeError> {
        let bytes = self.take(2, context)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self, context: &'static str) -> Result<u32, DecodeError> {
        let bytes = self.take(4, context)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self, context: &'static str) -> Result<u64, DecodeError> {
        let bytes = self.take(8, context)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

/// Incremental decoder for a sequence of ESPectre V8 frames.
///
/// Feed raw bytes with [`FrameStreamDecoder::push`], then call
/// [`FrameStreamDecoder::next_frame`] until it returns `Ok(None)`. That
/// `None` means either "no bytes yet" or "a frame is split and more input is
/// needed"; both are normal for a socket or chunked file read.
///
/// The decoder keeps session and counter state so it can enforce the same
/// invariants as ESPectre's reference receiver. It never searches for a magic
/// number to resynchronize: a corrupt frame is reported and parsing stops, which
/// prevents silently decoding garbage.
#[derive(Debug)]
pub struct FrameStreamDecoder {
    /// Unconsumed bytes. A frame may be split across several `push` calls.
    buffer: Vec<u8>,
    /// Absolute stream offset of `buffer[0]`, used in error messages.
    base_offset: u64,
    /// Session id learned from the first frame.
    session_id: Option<[u8; SESSION_ID_LEN]>,
    /// Last accepted `stream_sequence` (`None` before the first frame).
    previous_stream_sequence: Option<u64>,
    /// Last accepted `fresh_record_total` (`None` before the first frame).
    previous_fresh_record_total: Option<u64>,
    /// Last accepted `raw_drop_total`; counters never move backwards.
    previous_raw_drop_total: u64,
    /// Last accepted `raw_send_backpressure_total`.
    previous_backpressure_total: u64,
    /// Number of fully decoded frames, for diagnostics.
    frames_decoded: u64,
}

impl Default for FrameStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStreamDecoder {
    /// Create an empty decoder.
    pub fn new() -> Self {
        Self::with_capacity(HTTP_FRAME_PREFIX_LEN)
    }

    /// Create an empty decoder whose buffer can hold `capacity` bytes without
    /// reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            base_offset: 0,
            session_id: None,
            previous_stream_sequence: None,
            previous_fresh_record_total: None,
            previous_raw_drop_total: 0,
            previous_backpressure_total: 0,
            frames_decoded: 0,
        }
    }

    /// Append raw stream bytes. Frames may be split anywhere.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Decode the next complete frame.
    ///
    /// * `Ok(Some(packet))` — one frame was decoded and consumed.
    /// * `Ok(None)` — the buffer holds no complete frame yet; push more bytes or
    ///   call [`FrameStreamDecoder::finish`] at end of input.
    /// * `Err(error)` — the stream is malformed. The offending bytes are left in
    ///   the buffer so a caller can inspect them.
    pub fn next_frame(&mut self) -> Result<Option<CsiPacket>, DecodeError> {
        // The prefix itself is the smallest thing we can inspect.
        if self.buffer.len() < HTTP_FRAME_PREFIX_LEN {
            return Ok(None);
        }

        let frame_base = self.base_offset;
        let prefix = HttpFrameHeader::parse(&self.buffer[..HTTP_FRAME_PREFIX_LEN], frame_base)?;

        // Validate the envelope before waiting for the whole frame: the
        // reference receiver fails fast on a bad prefix instead of buffering.
        self.validate_prefix(&prefix, frame_base)?;

        let frame_len = prefix.frame_len();
        if self.buffer.len() < frame_len {
            // Complete prefix, incomplete body. Keep the bytes and wait.
            return Ok(None);
        }

        // The record parser only sees one complete frame. `frame_len` is bounded
        // by `MAX_RECORD_BYTES`, so this slice is small.
        let frame = &self.buffer[..frame_len];
        let (csi, iq_raw) = parse_record(frame, frame_base, &prefix)?;

        // Only after a fully successful decode do we advance the stream state.
        self.commit(&prefix, frame_len);

        // `prefix` is moved into the packet; no copy of the header is needed.
        Ok(Some(CsiPacket {
            frame: prefix,
            csi,
            iq_raw,
        }))
    }

    /// Check the envelope and cross-frame invariants for one prefix.
    ///
    /// This reproduces the conditions in ESPectre's `_consume_raw_frames()`.
    /// It does not mutate state; [`Self::commit`] does that after the complete
    /// frame has been validated.
    fn validate_prefix(&self, prefix: &HttpFrameHeader, base: u64) -> Result<(), DecodeError> {
        if prefix.magic != RAW_CSI_RESPONSE_MAGIC {
            return Err(DecodeError::BadFrameMagic {
                offset: base,
                found: prefix.magic,
            });
        }
        if prefix.version != RAW_CSI_PROTOCOL_VERSION {
            return Err(DecodeError::UnsupportedProtocolVersion {
                offset: base + 4,
                found: prefix.version,
            });
        }
        if prefix.record_version != RAW_CSI_RECORD_VERSION_V8 {
            return Err(DecodeError::UnsupportedRecordVersion {
                offset: base + 5,
                found: prefix.record_version,
            });
        }
        if prefix.header_len != HTTP_FRAME_PREFIX_LEN_U16 {
            return Err(DecodeError::BadFrameHeaderLength {
                offset: base + 6,
                found: prefix.header_len,
                expected: HTTP_FRAME_PREFIX_LEN_U16,
            });
        }

        // The session id must stay the same for the whole connection.
        if let Some(expected) = self.session_id
            && prefix.session_id != expected
        {
            return Err(DecodeError::SessionChanged {
                offset: base + 8,
                expected,
                found: prefix.session_id,
            });
        }

        // `stream_sequence` is one-based and must strictly increase. Before the
        // first frame the reference receiver's previous value is 0, so 0 is
        // rejected too.
        match self.previous_stream_sequence {
            Some(previous) if prefix.stream_sequence <= previous => {
                return Err(DecodeError::StreamSequenceNotIncreasing {
                    offset: base + 24,
                    previous,
                    found: prefix.stream_sequence,
                });
            }
            None if prefix.stream_sequence == 0 => {
                return Err(DecodeError::StreamSequenceNotIncreasing {
                    offset: base + 24,
                    previous: 0,
                    found: 0,
                });
            }
            _ => {}
        }

        // Reserved by the format; a nonzero value would mean a protocol change
        // this decoder does not understand.
        if prefix.flags != 0 {
            return Err(DecodeError::NonZeroFrameFlags {
                offset: base + 34,
                found: prefix.flags,
            });
        }

        if prefix.record_len == 0 || prefix.record_len > MAX_RECORD_BYTES {
            return Err(DecodeError::RecordLengthOutOfRange {
                offset: base + 32,
                found: prefix.record_len,
                maximum: MAX_RECORD_BYTES,
            });
        }

        // A live session sends exactly one fresh record per frame. Captures
        // start at the beginning of a session, so the first value must be 1.
        let expected_fresh = self
            .previous_fresh_record_total
            .map_or(1, |previous| previous.saturating_add(1));
        if prefix.fresh_record_total != expected_fresh {
            return Err(DecodeError::FreshTotalNotSequential {
                offset: base + 36,
                previous: self.previous_fresh_record_total.unwrap_or(0),
                found: prefix.fresh_record_total,
            });
        }

        // Drop and backpressure totals are cumulative, so they may only grow.
        if prefix.raw_drop_total < self.previous_raw_drop_total {
            return Err(DecodeError::DropCounterWentBackwards {
                offset: base + 44,
                previous: self.previous_raw_drop_total,
                found: prefix.raw_drop_total,
            });
        }
        if prefix.raw_send_backpressure_total < self.previous_backpressure_total {
            return Err(DecodeError::BackpressureCounterWentBackwards {
                offset: base + 52,
                previous: self.previous_backpressure_total,
                found: prefix.raw_send_backpressure_total,
            });
        }

        Ok(())
    }

    /// Advance the per-stream state after one frame was fully decoded.
    fn commit(&mut self, prefix: &HttpFrameHeader, frame_len: usize) {
        if self.session_id.is_none() {
            self.session_id = Some(prefix.session_id);
        }
        self.previous_stream_sequence = Some(prefix.stream_sequence);
        self.previous_fresh_record_total = Some(prefix.fresh_record_total);
        self.previous_raw_drop_total = prefix.raw_drop_total;
        self.previous_backpressure_total = prefix.raw_send_backpressure_total;

        // Drop the consumed frame. The buffer only ever holds one partial frame
        // plus whatever arrived with it, so this stays cheap.
        self.buffer.drain(..frame_len);
        self.base_offset += frame_len as u64;
        self.frames_decoded += 1;
    }

    /// Report input that was not consumed.
    ///
    /// Call this after [`Self::next_frame`] has returned `Ok(None)` because the
    /// input ended. A clean stream ends exactly on a frame boundary and returns
    /// `Ok(())`. Otherwise the error distinguishes a truncated final frame from
    /// stray bytes.
    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // If a full prefix is present, we can say how many bytes are missing
        // from the final frame instead of only reporting leftover data.
        if self.buffer.len() >= HTTP_FRAME_PREFIX_LEN
            && let Ok(prefix) =
                HttpFrameHeader::parse(&self.buffer[..HTTP_FRAME_PREFIX_LEN], self.base_offset)
        {
            let needed = prefix.frame_len();
            if self.buffer.len() < needed {
                return Err(DecodeError::Truncated {
                    context: "HTTP frame body",
                    offset: self.base_offset,
                    needed,
                    available: self.buffer.len(),
                });
            }
        }

        Err(DecodeError::TrailingBytes {
            offset: self.base_offset,
            count: self.buffer.len(),
        })
    }

    /// Session id learned from the first decoded frame, if any.
    pub fn session_id(&self) -> Option<[u8; SESSION_ID_LEN]> {
        self.session_id
    }

    /// Number of frames decoded so far.
    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    /// Absolute offset of the first unconsumed byte.
    pub fn bytes_consumed(&self) -> u64 {
        self.base_offset
    }

    /// Bytes currently buffered but not yet decoded.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

/// Parse the 64-byte record header and payload out of one complete frame.
///
/// `frame` must start at the HTTP prefix and be exactly `prefix.frame_len()`
/// bytes long; [`FrameStreamDecoder::next_frame`] guarantees both.
///
/// The checks and their order follow `parse_csi_record()` in
/// `tools/lib/csi_io.py`. In addition, the record size must equal the size the
/// envelope declared, which the reference receiver enforces separately.
fn parse_record(
    frame: &[u8],
    frame_base: u64,
    prefix: &HttpFrameHeader,
) -> Result<(CsiHeader, Vec<i8>), DecodeError> {
    // The record starts right after the prefix.
    let record_base = frame_base + HTTP_FRAME_PREFIX_LEN as u64;
    let mut r = LeReader::new(&frame[HTTP_FRAME_PREFIX_LEN..], record_base);

    // Field order must match `<HBBBBIHHQQIQBbbQIIBBB` exactly.
    let magic = r.u16("CSI record magic")?;
    let version = r.u8("CSI record version")?;
    let header_len = r.u8("CSI record header length")?;
    let chip_code = r.u8("CSI record chip code")?;
    let flags = r.u8("CSI record flags")?;
    let seq_num = r.u32("CSI record sequence")?;
    let num_sc = r.u16("CSI record subcarrier count")?;
    let csi_len_bytes = r.u16("CSI record payload length")?;
    let device_id = r.u64("CSI record device id")?;
    let device_ticks_us = r.u64("CSI record device ticks")?;
    let wifi_rx_ts_us = r.u32("CSI record Wi-Fi RX timestamp")?;
    let wifi_rx_start_ts_ns = r.u64("CSI record Wi-Fi RX start timestamp")?;
    let channel = r.u8("CSI record channel")?;
    let rssi_dbm = r.i8("CSI record RSSI")?;
    let noise_floor_dbm = r.i8("CSI record noise floor")?;
    let transport_counter = r.u64("CSI record transport counter")?;
    let fresh_counter = r.u32("CSI record fresh counter")?;
    let request_counter = r.u32("CSI record request counter")?;
    let phy_mode_code = r.u8("CSI record PHY mode")?;
    let ltf_type_code = r.u8("CSI record LTF type")?;
    let channel_width_code = r.u8("CSI record channel width")?;

    // --- Record-local validation (mirrors parse_csi_record) ----------------
    if magic != RAW_CSI_RECORD_MAGIC {
        return Err(DecodeError::BadRecordMagic {
            offset: record_base,
            found: magic,
        });
    }
    if version != RAW_CSI_RECORD_VERSION_V8 {
        return Err(DecodeError::UnsupportedRecordVersion {
            offset: record_base + 2,
            found: version,
        });
    }
    if header_len < CSI_HEADER_LEN {
        return Err(DecodeError::RecordHeaderTooShort {
            offset: record_base + 3,
            found: header_len,
            minimum: CSI_HEADER_LEN,
        });
    }
    if csi_len_bytes == 0 {
        return Err(DecodeError::ZeroPayloadLength {
            offset: record_base + 12,
        });
    }
    // Use u32 for the multiplication so a large `num_sc` cannot overflow u16.
    if u32::from(csi_len_bytes) != u32::from(num_sc) * 2 {
        return Err(DecodeError::PayloadLengthMismatch {
            offset: record_base + 12,
            num_subcarriers: num_sc,
            payload_len: csi_len_bytes,
        });
    }
    if csi_len_bytes > MAX_PAYLOAD_BYTES {
        return Err(DecodeError::PayloadTooLarge {
            offset: record_base + 12,
            found: csi_len_bytes,
            maximum: MAX_PAYLOAD_BYTES,
        });
    }

    // The envelope's record_len must account for exactly this record. This is
    // the "next_offset == len(frame)" check in the reference receiver.
    let declared_record_len = u32::from(header_len) + u32::from(csi_len_bytes);
    if u32::from(prefix.record_len) != declared_record_len {
        return Err(DecodeError::RecordLengthMismatch {
            offset: record_base,
            frame_record_len: prefix.record_len,
            record_header_len: header_len,
            payload_len: csi_len_bytes,
        });
    }

    // --- Cross-checks between envelope and record -------------------------
    // The record duplicates three envelope counters so a receiver can detect a
    // desynchronized stream. Saturation at u32::MAX is expected on long runs.
    let saturated_stream_sequence = prefix.stream_sequence.min(u64::from(u32::MAX)) as u32;
    if seq_num != saturated_stream_sequence {
        return Err(DecodeError::CounterMismatch {
            offset: record_base + 6,
            field: "seq_num",
            expected: u64::from(saturated_stream_sequence),
            found: u64::from(seq_num),
        });
    }
    if request_counter != saturated_stream_sequence {
        return Err(DecodeError::CounterMismatch {
            offset: record_base + 57,
            field: "request_counter",
            expected: u64::from(saturated_stream_sequence),
            found: u64::from(request_counter),
        });
    }
    let saturated_fresh_total = prefix.fresh_record_total.min(u64::from(u32::MAX)) as u32;
    if fresh_counter != saturated_fresh_total {
        return Err(DecodeError::CounterMismatch {
            offset: record_base + 53,
            field: "fresh_counter",
            expected: u64::from(saturated_fresh_total),
            found: u64::from(fresh_counter),
        });
    }
    if transport_counter != prefix.raw_send_backpressure_total {
        return Err(DecodeError::CounterMismatch {
            offset: record_base + 45,
            field: "transport_counter",
            expected: prefix.raw_send_backpressure_total,
            found: transport_counter,
        });
    }

    // --- Payload ----------------------------------------------------------
    // A future record version may extend the header; honour `header_len` so the
    // payload still starts at the declared place.
    r.skip(
        usize::from(header_len) - usize::from(CSI_HEADER_LEN),
        "CSI record header extension",
    )?;
    let payload = r.take(usize::from(csi_len_bytes), "CSI payload")?;

    // Copy the bytes without reordering or normalizing. `as i8` keeps the exact
    // two's-complement bit pattern of the signed wire samples.
    let iq_raw = payload.iter().map(|&byte| byte as i8).collect();

    let csi = CsiHeader {
        magic,
        version,
        header_len,
        chip_code,
        flags,
        seq_num,
        num_sc,
        csi_len_bytes,
        device_id,
        device_ticks_us,
        wifi_rx_ts_us,
        wifi_rx_start_ts_ns,
        channel,
        rssi_dbm,
        noise_floor_dbm,
        transport_counter,
        fresh_counter,
        request_counter,
        phy_mode_code,
        ltf_type_code,
        channel_width_code,
    };

    Ok((csi, iq_raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csi_header::{RAW_CSI_FLAG_FRESH, RAW_CSI_FLAG_WIFI_RX_TS_VALID};

    /// Session id used by the synthetic frames.
    const SESSION: [u8; SESSION_ID_LEN] = [0x11; SESSION_ID_LEN];

    /// Overwrite a little-endian u16 field in a test frame.
    fn set_u16(buf: &mut [u8], offset: usize, value: u16) {
        buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Overwrite a little-endian u32 field in a test frame.
    fn set_u32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Overwrite a little-endian u64 field in a test frame.
    fn set_u64(buf: &mut [u8], offset: usize, value: u64) {
        buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// Build a 60-byte prefix with the given counters.
    fn build_prefix(
        stream_sequence: u64,
        fresh_record_total: u64,
        record_len: u16,
        raw_drop_total: u64,
        backpressure_total: u64,
        session_id: [u8; SESSION_ID_LEN],
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(HTTP_FRAME_PREFIX_LEN);
        out.extend_from_slice(&RAW_CSI_RESPONSE_MAGIC.to_le_bytes());
        out.push(RAW_CSI_PROTOCOL_VERSION);
        out.push(RAW_CSI_RECORD_VERSION_V8);
        out.extend_from_slice(&HTTP_FRAME_PREFIX_LEN_U16.to_le_bytes());
        out.extend_from_slice(&session_id);
        out.extend_from_slice(&stream_sequence.to_le_bytes());
        out.extend_from_slice(&record_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved flags
        out.extend_from_slice(&fresh_record_total.to_le_bytes());
        out.extend_from_slice(&raw_drop_total.to_le_bytes());
        out.extend_from_slice(&backpressure_total.to_le_bytes());
        debug_assert_eq!(out.len(), HTTP_FRAME_PREFIX_LEN);
        out
    }

    /// Build a 64-byte record header plus payload with self-consistent fields.
    fn build_record(seq_num: u32, payload: &[i8]) -> Vec<u8> {
        let num_sc = (payload.len() / 2) as u16;
        let csi_len = payload.len() as u16;
        let mut out = Vec::with_capacity(usize::from(CSI_HEADER_LEN) + payload.len());
        out.extend_from_slice(&RAW_CSI_RECORD_MAGIC.to_le_bytes());
        out.push(RAW_CSI_RECORD_VERSION_V8);
        out.push(CSI_HEADER_LEN);
        out.push(5); // chip: C5
        out.push(RAW_CSI_FLAG_WIFI_RX_TS_VALID | RAW_CSI_FLAG_FRESH);
        out.extend_from_slice(&seq_num.to_le_bytes());
        out.extend_from_slice(&num_sc.to_le_bytes());
        out.extend_from_slice(&csi_len.to_le_bytes());
        out.extend_from_slice(&0xb78d_3a15_354a_d161u64.to_le_bytes());
        out.extend_from_slice(&1_000_000u64.to_le_bytes());
        out.extend_from_slice(&999_000u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.push(149);
        out.push((-35i8) as u8);
        out.push((-94i8) as u8);
        out.extend_from_slice(&0u64.to_le_bytes()); // transport counter
        out.extend_from_slice(&seq_num.to_le_bytes()); // fresh counter
        out.extend_from_slice(&seq_num.to_le_bytes()); // request counter
        out.push(3); // PHY: VHT
        out.push(3); // LTF: VHT-LTF
        out.push(1); // width: 20 MHz
        for &sample in payload {
            out.push(sample as u8);
        }
        debug_assert_eq!(out.len(), usize::from(CSI_HEADER_LEN) + payload.len());
        out
    }

    /// Build one complete frame with explicit envelope counters.
    fn build_frame(
        stream_sequence: u64,
        fresh_record_total: u64,
        raw_drop_total: u64,
        backpressure_total: u64,
        payload: &[i8],
    ) -> Vec<u8> {
        let mut record = build_record(stream_sequence as u32, payload);
        // The record mirrors the envelope's backpressure total.
        set_u64(&mut record, 45, backpressure_total);
        let prefix = build_prefix(
            stream_sequence,
            fresh_record_total,
            record.len() as u16,
            raw_drop_total,
            backpressure_total,
            SESSION,
        );
        let mut out = prefix;
        out.extend_from_slice(&record);
        out
    }

    /// Build a plain valid frame where every counter is `seq`.
    fn valid_frame(seq: u64, payload: &[i8]) -> Vec<u8> {
        build_frame(seq, seq, 0, 0, payload)
    }

    /// Decode every byte in `bytes`, asserting a clean end of stream.
    fn decode_all(bytes: &[u8]) -> Vec<CsiPacket> {
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(bytes);
        let mut packets = Vec::new();
        while let Some(packet) = decoder.next_frame().expect("frame should decode") {
            packets.push(packet);
        }
        decoder.finish().expect("stream should end cleanly");
        packets
    }

    #[test]
    fn decodes_one_valid_frame() {
        let payload = [1, 2, 3, 4];
        let packets = decode_all(&valid_frame(1, &payload));
        assert_eq!(packets.len(), 1);

        let packet = &packets[0];
        assert_eq!(packet.frame.stream_sequence, 1);
        assert_eq!(packet.frame.record_len, 64 + 4);
        assert_eq!(packet.frame.session_id, SESSION);
        assert_eq!(packet.csi.magic, RAW_CSI_RECORD_MAGIC);
        assert_eq!(packet.csi.version, RAW_CSI_RECORD_VERSION_V8);
        assert_eq!(packet.csi.header_len, CSI_HEADER_LEN);
        assert_eq!(packet.csi.num_sc, 2);
        assert_eq!(packet.csi.csi_len_bytes, 4);
        assert_eq!(packet.csi.device_id, 0xb78d_3a15_354a_d161);
        assert_eq!(packet.csi.channel, 149);
        assert_eq!(packet.csi.rssi_dbm, -35);
        assert_eq!(packet.csi.noise_floor_dbm, -94);
        assert_eq!(packet.csi.chip_name(), "C5");
        assert_eq!(packet.csi.phy_mode_name(), "vht");
        assert_eq!(packet.csi.ltf_type_name(), "vht-ltf");
        assert_eq!(packet.csi.channel_width_name(), "20");
        assert!(packet.csi.is_fresh());
        assert_eq!(packet.valid_wifi_rx_ts_us(), Some(999_000));
        assert_eq!(packet.valid_noise_floor_dbm(), Some(-94));
        assert_eq!(packet.iq_raw, payload);
    }

    #[test]
    fn derives_i_and_q_in_reference_order() {
        // ESPectre's test fixture: [10, 20, -30, 40] -> [20+10j, 40-30j].
        let packets = decode_all(&valid_frame(1, &[10, 20, -30, 40]));
        let packet = &packets[0];

        let pairs: Vec<(f32, f32)> = packet.iq_pairs().collect();
        assert_eq!(pairs, vec![(20.0, 10.0), (40.0, -30.0)]);

        let amplitudes = packet.amplitudes();
        assert!((amplitudes[0] - 500.0f32.sqrt()).abs() < 1e-6);
        assert!((amplitudes[1] - 2500.0f32.sqrt()).abs() < 1e-6);

        let phases = packet.phases();
        assert!((phases[0] - 10.0f32.atan2(20.0)).abs() < 1e-6);
    }

    #[test]
    fn handles_a_frame_split_across_pushes() {
        let frame = valid_frame(1, &[5, -5]);
        let mut decoder = FrameStreamDecoder::new();

        decoder.push(&frame[..10]);
        assert!(decoder.next_frame().expect("prefix incomplete").is_none());

        decoder.push(&frame[10..100]);
        assert!(decoder.next_frame().expect("record incomplete").is_none());

        decoder.push(&frame[100..]);
        let packet = decoder
            .next_frame()
            .expect("split frame should decode")
            .expect("frame is complete");
        assert_eq!(packet.csi.seq_num, 1);
        assert!(decoder.next_frame().expect("no second frame").is_none());
        decoder.finish().expect("clean end");
    }

    #[test]
    fn handles_two_frames_in_one_push() {
        let mut bytes = valid_frame(1, &[1, 2]);
        bytes.extend_from_slice(&valid_frame(2, &[3, 4]));

        let packets = decode_all(&bytes);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].csi.seq_num, 1);
        assert_eq!(packets[1].csi.seq_num, 2);
        assert_eq!(packets[0].frame.stream_sequence, 1);
        assert_eq!(packets[1].frame.stream_sequence, 2);
    }

    #[test]
    fn rejects_bad_frame_magic() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u32(&mut frame, 0, 0xdead_beef);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::BadFrameMagic { offset: 0, .. })
        ));
    }

    #[test]
    fn rejects_unsupported_protocol_version() {
        let mut frame = valid_frame(1, &[1, 2]);
        frame[4] = 2;
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::UnsupportedProtocolVersion { offset: 4, .. })
        ));
    }

    #[test]
    fn rejects_unsupported_record_version_in_prefix() {
        let mut frame = valid_frame(1, &[1, 2]);
        frame[5] = 7;
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::UnsupportedRecordVersion { offset: 5, .. })
        ));
    }

    #[test]
    fn rejects_bad_prefix_length() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 6, 64);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::BadFrameHeaderLength { offset: 6, .. })
        ));
    }

    #[test]
    fn rejects_a_changed_session_id() {
        let mut bytes = valid_frame(1, &[1, 2]);
        // Second frame, different session id.
        let mut second = build_prefix(2, 2, 66, 0, 0, [0x22; SESSION_ID_LEN]);
        second.extend_from_slice(&build_record(2, &[3, 4]));
        bytes.extend_from_slice(&second);

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("first frame").is_some());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::SessionChanged { .. })
        ));
    }

    #[test]
    fn rejects_stream_sequence_that_does_not_increase() {
        let mut bytes = valid_frame(1, &[1, 2]);
        bytes.extend_from_slice(&valid_frame(1, &[3, 4]));

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("first frame").is_some());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::StreamSequenceNotIncreasing {
                previous: 1,
                found: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_zero_stream_sequence_on_first_frame() {
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&build_frame(0, 1, 0, 0, &[1, 2]));
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::StreamSequenceNotIncreasing { .. })
        ));
    }

    #[test]
    fn rejects_nonzero_frame_flags() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 34, 1);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::NonZeroFrameFlags { offset: 34, .. })
        ));
    }

    #[test]
    fn rejects_zero_record_length() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 32, 0);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::RecordLengthOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_oversized_record_length() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 32, MAX_RECORD_BYTES + 1);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::RecordLengthOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_fresh_total_jump() {
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&build_frame(1, 2, 0, 0, &[1, 2]));
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::FreshTotalNotSequential {
                previous: 0,
                found: 2,
                ..
            })
        ));
    }

    #[test]
    fn rejects_fresh_total_that_does_not_advance_by_one() {
        let mut bytes = valid_frame(1, &[1, 2]);
        bytes.extend_from_slice(&build_frame(2, 4, 0, 0, &[3, 4]));

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("first frame").is_some());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::FreshTotalNotSequential {
                previous: 1,
                found: 4,
                ..
            })
        ));
    }

    #[test]
    fn rejects_drop_counter_going_backwards() {
        let mut bytes = build_frame(1, 1, 5, 0, &[1, 2]);
        bytes.extend_from_slice(&build_frame(2, 2, 3, 0, &[3, 4]));

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("first frame").is_some());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::DropCounterWentBackwards {
                previous: 5,
                found: 3,
                ..
            })
        ));
    }

    #[test]
    fn rejects_backpressure_counter_going_backwards() {
        let mut bytes = build_frame(1, 1, 0, 5, &[1, 2]);
        bytes.extend_from_slice(&build_frame(2, 2, 0, 3, &[3, 4]));

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("first frame").is_some());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::BackpressureCounterWentBackwards {
                previous: 5,
                found: 3,
                ..
            })
        ));
    }

    #[test]
    fn rejects_bad_record_magic() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 60, 0x1234);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::BadRecordMagic { offset: 60, .. })
        ));
    }

    #[test]
    fn rejects_short_record_header() {
        let mut frame = valid_frame(1, &[1, 2]);
        frame[63] = 60;
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::RecordHeaderTooShort { found: 60, .. })
        ));
    }

    #[test]
    fn rejects_zero_payload_length() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 70, 0); // num_sc
        set_u16(&mut frame, 72, 0); // csi_len
        set_u16(&mut frame, 32, 64); // envelope record_len: header only
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::ZeroPayloadLength { .. })
        ));
    }

    #[test]
    fn rejects_payload_length_mismatch() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 70, 64); // num_sc says 64 ...
        set_u16(&mut frame, 72, 4); // ... but payload is 4 bytes
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::PayloadLengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_oversized_payload() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 70, 257); // 257 * 2 == 514 > 512
        set_u16(&mut frame, 72, 514);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::PayloadTooLarge { found: 514, .. })
        ));
    }

    #[test]
    fn rejects_record_length_mismatch() {
        // Envelope claims only the 64-byte header, but the record also carries
        // a 2-byte payload.
        let mut frame = valid_frame(1, &[1, 2]);
        set_u16(&mut frame, 32, 64);
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::RecordLengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_counter_mismatch_between_layers() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u32(&mut frame, 66, 99); // record seq_num
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::CounterMismatch {
                field: "seq_num",
                expected: 1,
                found: 99,
                ..
            })
        ));
    }

    #[test]
    fn rejects_transport_counter_mismatch() {
        let mut frame = valid_frame(1, &[1, 2]);
        set_u64(&mut frame, 105, 7); // record transport counter (60 + 45)
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::CounterMismatch {
                field: "transport_counter",
                ..
            })
        ));
    }

    #[test]
    fn rejects_nonzero_record_version() {
        let mut frame = valid_frame(1, &[1, 2]);
        frame[62] = 7;
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::UnsupportedRecordVersion { offset: 62, .. })
        ));
    }

    #[test]
    fn reports_trailing_bytes() {
        let mut bytes = valid_frame(1, &[1, 2]);
        bytes.extend_from_slice(&[0xaa, 0xbb, 0xcc]);

        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&bytes);
        assert!(decoder.next_frame().expect("frame").is_some());
        assert!(decoder.next_frame().expect("no frame").is_none());
        assert!(matches!(
            decoder.finish(),
            Err(DecodeError::TrailingBytes { count: 3, .. })
        ));
    }

    #[test]
    fn reports_truncated_final_frame() {
        let frame = valid_frame(1, &[1, 2]);
        let mut decoder = FrameStreamDecoder::new();
        // Complete prefix, incomplete record.
        decoder.push(&frame[..70]);
        assert!(decoder.next_frame().expect("waiting for body").is_none());
        assert!(matches!(
            decoder.finish(),
            Err(DecodeError::Truncated { needed, available: 70, .. }) if needed == 126
        ));
    }

    #[test]
    fn treats_zero_noise_floor_sentinel_as_unknown() {
        let mut frame = valid_frame(1, &[1, 2]);
        frame[104] = (-128i8) as u8; // 60 + 44
        let mut decoder = FrameStreamDecoder::new();
        decoder.push(&frame);
        let packet = decoder.next_frame().expect("frame").expect("complete");
        assert_eq!(packet.csi.noise_floor_dbm, -128);
        assert_eq!(packet.valid_noise_floor_dbm(), None);
    }

    #[test]
    fn exposes_zero_based_counters() {
        let mut decoder = FrameStreamDecoder::new();
        assert_eq!(decoder.frames_decoded(), 0);
        assert_eq!(decoder.bytes_consumed(), 0);
        assert!(decoder.session_id().is_none());

        decoder.push(&valid_frame(1, &[1, 2]));
        assert!(decoder.next_frame().expect("frame").is_some());
        assert_eq!(decoder.frames_decoded(), 1);
        assert_eq!(decoder.bytes_consumed(), 126); // 60-byte prefix + 66-byte record
        assert_eq!(decoder.session_id(), Some(SESSION));
    }
}
