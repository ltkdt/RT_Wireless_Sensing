//! End-to-end tests against the real ESPectre capture.
//!
//! `capture_v8.bin` was recorded from an ESP32-C5 at 20 PPS and validated with
//! ESPectre's Python reference parser. These tests lock in the whole-file
//! result so a change to the decoder cannot silently alter record boundaries,
//! counters, or the raw I/Q payload.

use std::{fs, path::Path};

use rt_wireless_sensing::decoder::{CsiPacket, FrameStreamDecoder};

/// Load and fully decode `capture_v8.bin`.
fn decode_capture() -> Vec<CsiPacket> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("capture_v8.bin");
    let bytes = fs::read(&path).expect("capture_v8.bin should exist");
    let mut decoder = FrameStreamDecoder::new();
    decoder.push(&bytes);

    let mut packets = Vec::new();
    while let Some(packet) = decoder.next_frame().expect("every frame should decode") {
        packets.push(packet);
    }
    decoder
        .finish()
        .expect("capture should end on a frame boundary");
    packets
}

#[test]
fn decodes_all_691_records() {
    let packets = decode_capture();
    assert_eq!(packets.len(), 691);
}

#[test]
fn first_record_matches_the_hex_dump() {
    let packets = decode_capture();
    let first = &packets[0];

    assert_eq!(first.frame.magic, 0x5250_5345);
    assert_eq!(first.frame.version, 1);
    assert_eq!(first.frame.record_version, 8);
    assert_eq!(first.frame.header_len, 60);
    assert_eq!(
        first.frame.session_id,
        [
            0xd9, 0x5c, 0x15, 0xbb, 0xda, 0xe4, 0x09, 0xe6, 0xaf, 0xdc, 0x31, 0x91, 0x01, 0x6c,
            0xad, 0xbb,
        ]
    );
    assert_eq!(first.frame.stream_sequence, 1);
    assert_eq!(first.frame.record_len, 192);
    assert_eq!(first.frame.flags, 0);
    assert_eq!(first.frame.fresh_record_total, 1);
    assert_eq!(first.frame.raw_drop_total, 0);
    assert_eq!(first.frame.raw_send_backpressure_total, 0);

    assert_eq!(first.csi.magic, 0x4353);
    assert_eq!(first.csi.version, 8);
    assert_eq!(first.csi.header_len, 64);
    assert_eq!(first.csi.chip_code, 5);
    assert_eq!(first.csi.chip_name(), "C5");
    assert_eq!(first.csi.flags, 0b1010);
    assert!(first.csi.is_fresh());
    assert_eq!(first.csi.seq_num, 1);
    assert_eq!(first.csi.num_sc, 64);
    assert_eq!(first.csi.csi_len_bytes, 128);
    assert_eq!(first.csi.device_id, 0xb78d_3a15_354a_d161);
    assert_eq!(first.csi.device_ticks_us, 788_187_556);
    assert_eq!(first.csi.wifi_rx_ts_us, 788_134_401);
    assert_eq!(first.csi.channel, 149);
    assert_eq!(first.csi.rssi_dbm, -35);
    assert_eq!(first.csi.noise_floor_dbm, -94);
    assert_eq!(first.csi.phy_mode_code, 3);
    assert_eq!(first.csi.phy_mode_name(), "vht");
    assert_eq!(first.csi.ltf_type_name(), "vht-ltf");
    assert_eq!(first.csi.channel_width_name(), "20");

    // Payload: bins 0-3 are guard/DC nulls, bin 4 is the first live sample.
    assert_eq!(&first.iq_raw[..8], &[0i8, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&first.iq_raw[8..10], &[5i8, -20]);
}

#[test]
fn raw_payload_is_preserved_exactly() {
    let packets = decode_capture();

    // Golden values computed with the Python reference parser.
    let total: i64 = packets
        .iter()
        .flat_map(|packet| packet.iq_raw.iter())
        .map(|&sample| i64::from(sample))
        .sum();
    assert_eq!(total, -42_069);

    // First subcarrier amplitude and phase, from (I, Q) = (-20, 5).
    let amplitude = packets[0].amplitudes()[4];
    let phase = packets[0].phases()[4];
    assert!(
        (amplitude - 20.615_528).abs() < 1e-3,
        "amplitude {amplitude}"
    );
    assert!((phase - 2.896_614).abs() < 1e-4, "phase {phase}");

    // The centered HT20 layout always has these guard/DC bins null.
    for packet in &packets {
        for bin in [0usize, 1, 2, 3, 32, 61, 62, 63] {
            assert_eq!(&packet.iq_raw[bin * 2..bin * 2 + 2], &[0i8, 0]);
        }
    }
}

#[test]
fn sequence_numbers_are_dense_across_the_capture() {
    let packets = decode_capture();

    for (index, pair) in packets.windows(2).enumerate() {
        let previous = pair[0].frame.stream_sequence;
        let current = pair[1].frame.stream_sequence;
        assert_eq!(
            current,
            previous + 1,
            "sequence gap after record {index}: {previous} -> {current}"
        );
        assert_eq!(pair[1].csi.seq_num, current as u32);
        assert_eq!(pair[1].frame.fresh_record_total, current);
    }

    assert_eq!(packets.first().unwrap().frame.stream_sequence, 1);
    assert_eq!(packets.last().unwrap().frame.stream_sequence, 691);
    assert_eq!(packets.last().unwrap().csi.device_ticks_us, 822_686_631);
}

#[test]
fn decoder_accepts_input_split_at_arbitrary_boundaries() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("capture_v8.bin");
    let bytes = fs::read(&path).expect("capture_v8.bin should exist");

    // Odd chunk sizes force frames to straddle `push` calls.
    let mut decoder = FrameStreamDecoder::new();
    let mut packets = Vec::new();
    for chunk in bytes.chunks(37) {
        decoder.push(chunk);
        while let Some(packet) = decoder.next_frame().expect("split input should decode") {
            packets.push(packet);
        }
    }
    decoder.finish().expect("clean end");

    assert_eq!(packets.len(), 691);
    assert_eq!(decoder.frames_decoded(), 691);
    assert_eq!(decoder.bytes_consumed(), bytes.len() as u64);
}

#[test]
fn payload_hash_matches_the_python_reference() {
    // FNV-1a 64 over every raw I/Q byte, computed independently by the
    // ESPectre Python parser. A cheap whole-payload equality check that needs
    // no extra dependency.
    let packets = decode_capture();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for packet in &packets {
        for &sample in &packet.iq_raw {
            hash ^= u64::from(sample as u8);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    assert_eq!(hash, 0x6446_fc78_e2c7_f3e6);
}
