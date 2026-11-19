//! ESPectre V8 CSI record schema.
//!
//! On the `/espectre/v1/csi` stream every record is:
//!
//! ```text
//! 60-byte HTTP frame prefix │ 64-byte CSI record header │ csi_len_bytes payload
//! ```
//!
//! This module defines the 64-byte header only. The 60-byte transport prefix
//! and all parsing/validation live in [`crate::decoder`], so the two layers
//! cannot be confused with each other.
//!
//! The layout mirrors ESPectre's `RawCsiRecordHeaderV8` in
//! `src/cpp/runtime/csi_raw_record.h` and the Python
//! `CSI_HEADER_STRUCT = struct.Struct("<HBBBBIHHQQIQBbbQIIBBB")`.
//! That format is little-endian, uses standard sizes, and has no alignment
//! padding, which is why the fields below pack into exactly 64 bytes.

/// Size of the V8 CSI record header in bytes.
///
/// The wire format allows a larger `header_len` (future versions may append
/// fields), but 64 is the size this schema describes and the size current
/// firmware sends.
pub const CSI_HEADER_LEN: u8 = 64;

/// `magic` value of a CSI record: the ASCII bytes `"SC"` on the wire.
pub const RAW_CSI_RECORD_MAGIC: u16 = 0x4353;

/// Record version this decoder understands.
pub const RAW_CSI_RECORD_VERSION_V8: u8 = 8;

/// Noise-floor sentinel meaning "the device could not measure it".
///
/// It is far below the thermal noise of a 20 MHz channel, so it can never be a
/// genuine reading. `noise_floor_dbm == -128` must be surfaced as "unknown".
pub const NOISE_FLOOR_INVALID_DBM: i8 = -128;

/// Largest CSI payload a V8 record may carry.
///
/// The built-in pipeline sends 128 bytes (64 complex subcarriers); the format
/// limit is 512 bytes. The envelope's `record_len <= 64 + 512` encodes the same
/// bound, and the decoder checks both for clearer errors.
pub const MAX_PAYLOAD_BYTES: u16 = 512;

/// Number of complex subcarriers in the standard ESPectre HT20 capture.
///
/// This schema owns the wire layout, so the canonical subcarrier count lives
/// here. The decoder does not assume this value — it uses each record's
/// `num_sc` — but current firmware sends 64, and the payload is already
/// normalized to the centered 64-subcarrier layout. Modules that need to
/// predict a normal capture's shape (for example "129 arrays in the `.npz`")
/// should use this constant instead of duplicating the literal.
pub const STANDARD_SUBCARRIERS: usize = 64;

// --- Bits of `CsiHeader::flags` (see `RawCsiRecordFlags`) -------------------

/// The hardware marked the first four payload bytes invalid.
///
/// The runtime still emits full-width frames with a known layout and zeroes the
/// affected values; in the classic layout this hits the DC bin and subcarrier
/// +1, and in the centered layout it hits bins 0 and 1.
pub const RAW_CSI_FLAG_FIRST_WORD_INVALID: u8 = 1 << 0;

/// `wifi_rx_ts_us` holds a driver receive timestamp and may be used.
pub const RAW_CSI_FLAG_WIFI_RX_TS_VALID: u8 = 1 << 1;

/// `wifi_rx_start_ts_ns` holds a receive-start timestamp and may be used.
///
/// The SDK currently writes zero here even when the bit is set.
pub const RAW_CSI_FLAG_WIFI_RX_START_TS_NS_VALID: u8 = 1 << 2;

/// The record is a fresh CSI sample (set on every emitted record).
pub const RAW_CSI_FLAG_FRESH: u8 = 1 << 3;

/// One decoded V8 CSI record header.
///
/// The field order follows the wire format exactly; see the module docs. All
/// integers are stored in host byte order after the decoder has converted them
/// from little-endian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsiHeader {
    /// Must be [`RAW_CSI_RECORD_MAGIC`].
    pub magic: u16,
    /// Must be [`RAW_CSI_RECORD_VERSION_V8`].
    pub version: u8,
    /// Size of this header; at least [`CSI_HEADER_LEN`].
    pub header_len: u8,
    /// `RawCsiChipType` code; see [`chip_name`].
    pub chip_code: u8,
    /// Bit set of the `RAW_CSI_FLAG_*` constants above.
    pub flags: u8,

    /// Stream sequence, saturated by the device at `u32::MAX`.
    pub seq_num: u32,
    /// Complex subcarriers in the payload; equals `csi_len_bytes / 2`.
    pub num_sc: u16,
    /// CSI payload size in bytes.
    pub csi_len_bytes: u16,

    /// Device identity. `0` is treated as "unknown".
    pub device_id: u64,
    /// Monotonic device time captured with the CSI sample, in microseconds.
    pub device_ticks_us: u64,
    /// Wi-Fi driver receive timestamp in microseconds; only valid when
    /// [`RAW_CSI_FLAG_WIFI_RX_TS_VALID`] is set. On classic ESP32 it uses a
    /// different clock than `device_ticks_us`.
    pub wifi_rx_ts_us: u32,
    /// Receive-start timestamp in nanoseconds; only valid when
    /// [`RAW_CSI_FLAG_WIFI_RX_START_TS_NS_VALID`] is set.
    pub wifi_rx_start_ts_ns: u64,

    /// Primary Wi-Fi channel the packet arrived on.
    pub channel: u8,
    /// RSSI in dBm.
    pub rssi_dbm: i8,
    /// Noise floor in dBm; `-128` means unknown (see
    /// [`NOISE_FLOOR_INVALID_DBM`]).
    pub noise_floor_dbm: i8,

    /// Copy of the HTTP prefix's `raw_send_backpressure_total`.
    pub transport_counter: u64,
    /// Copy of the HTTP prefix's `fresh_record_total`, saturated at `u32::MAX`.
    pub fresh_counter: u32,
    /// Copy of the HTTP prefix's `stream_sequence`, saturated at `u32::MAX`.
    pub request_counter: u32,

    /// `RawCsiPhyMode` code; see [`phy_mode_name`].
    pub phy_mode_code: u8,
    /// `RawCsiLtfType` code; see [`ltf_type_name`].
    pub ltf_type_code: u8,
    /// `RawCsiChannelWidth` code; see [`channel_width_name`].
    pub channel_width_code: u8,
}

impl CsiHeader {
    /// Whether the hardware flagged the first four payload bytes as invalid.
    pub fn first_word_invalid(&self) -> bool {
        self.flags & RAW_CSI_FLAG_FIRST_WORD_INVALID != 0
    }

    /// Whether `wifi_rx_ts_us` is meaningful.
    pub fn wifi_rx_ts_valid(&self) -> bool {
        self.flags & RAW_CSI_FLAG_WIFI_RX_TS_VALID != 0
    }

    /// Whether `wifi_rx_start_ts_ns` is meaningful.
    pub fn wifi_rx_start_ts_ns_valid(&self) -> bool {
        self.flags & RAW_CSI_FLAG_WIFI_RX_START_TS_NS_VALID != 0
    }

    /// Whether this record is a fresh sample.
    pub fn is_fresh(&self) -> bool {
        self.flags & RAW_CSI_FLAG_FRESH != 0
    }

    /// The driver receive timestamp, or `None` when its validity bit is clear.
    pub fn valid_wifi_rx_ts_us(&self) -> Option<u32> {
        self.wifi_rx_ts_valid().then_some(self.wifi_rx_ts_us)
    }

    /// The receive-start timestamp, or `None` when its validity bit is clear.
    pub fn valid_wifi_rx_start_ts_ns(&self) -> Option<u64> {
        self.wifi_rx_start_ts_ns_valid()
            .then_some(self.wifi_rx_start_ts_ns)
    }

    /// The noise floor, or `None` when the device sent the `-128` sentinel.
    pub fn valid_noise_floor_dbm(&self) -> Option<i8> {
        (self.noise_floor_dbm != NOISE_FLOOR_INVALID_DBM).then_some(self.noise_floor_dbm)
    }

    /// Human-readable chip name for [`CsiHeader::chip_code`].
    pub fn chip_name(&self) -> &'static str {
        chip_name(self.chip_code)
    }

    /// Human-readable PHY name for [`CsiHeader::phy_mode_code`].
    pub fn phy_mode_name(&self) -> &'static str {
        phy_mode_name(self.phy_mode_code)
    }

    /// Human-readable training-field name for [`CsiHeader::ltf_type_code`].
    pub fn ltf_type_name(&self) -> &'static str {
        ltf_type_name(self.ltf_type_code)
    }

    /// Human-readable channel-width name for
    /// [`CsiHeader::channel_width_code`].
    pub fn channel_width_name(&self) -> &'static str {
        channel_width_name(self.channel_width_code)
    }
}

/// Map a `RawCsiChipType` code to a readable name.
pub fn chip_name(code: u8) -> &'static str {
    match code {
        0 => "unknown",
        1 => "ESP32",
        2 => "S2",
        3 => "S3",
        4 => "C3",
        5 => "C5",
        6 => "C6",
        _ => "unknown",
    }
}

/// Map a `RawCsiPhyMode` code to a readable name.
pub fn phy_mode_name(code: u8) -> &'static str {
    match code {
        0 => "unknown",
        1 => "legacy",
        2 => "ht",
        3 => "vht",
        4 => "he-su",
        5 => "he-mu",
        6 => "he-ersu",
        7 => "he-tb",
        _ => "unknown",
    }
}

/// Map a `RawCsiLtfType` code to a readable name.
pub fn ltf_type_name(code: u8) -> &'static str {
    match code {
        0 => "unknown",
        1 => "lltf",
        2 => "ht-ltf",
        3 => "vht-ltf",
        4 => "he-ltf",
        _ => "unknown",
    }
}

/// Map a `RawCsiChannelWidth` code to a readable width in MHz (or `+`).
pub fn channel_width_name(code: u8) -> &'static str {
    match code {
        0 => "unknown",
        1 => "20",
        2 => "40",
        3 => "80",
        4 => "160",
        5 => "80+80",
        _ => "unknown",
    }
}
