pub struct CsiHeader {
    pub magic: u16, // 2 bytes
    pub version: u8, // 1 byte
    pub header_len: u8, // 1 byte
    pub chip_code: u8, // 1 byte
    pub flags: u8,  // 1 byte

    pub seq_num: u32, // 4 bytes
    pub num_sc: u16, // 2 bytes
    pub csi_len_bytes: u16, // 2 bytes

    pub device_id: u64, // 8 bytes
    pub device_ticks_us: u64, // 8 bytes
    pub wifi_rx_ts_us: u32, // 4 bytes
    pub wifi_rx_start_ts_ns: u64,  // 8 bytes

    pub channel: u8,    // 1 byte
    pub rssi_dbm: i8,   // 1 byte
    pub noise_floor_dbm: i8,   // 1 byte

    pub transport_counter: u64, // 8 bytes
    pub fresh_counter: u32,   // 4 bytes
    pub request_counter: u32,   // 4 bytes

    pub phy_mode_code: u8,    // 1 byte
    pub ltf_type_code: u8,    // 1 byte
    pub channel_width_code: u8,   // 1 byte
    // Total: 64 bytes
}
