# Mô-đun giải mã CSI

Nguồn: [`src/decoder.rs`](../src/decoder.rs) · Nhị phân độc lập:
[`src/bin/decode.rs`](../src/bin/decode.rs) · Schema:
[`src/csi_header.rs`](../src/csi_header.rs)

Mô-đun này phân tích và xác thực dòng CSI V8 ESPectre. Nó xử lý các khung
chia trên nhiều lần đọc, nhiều khung mỗi lần đọc, và báo cáo hỏng hóc với offset byte tuyệt đối
thay vì đồng bộ lại im lặng.

## Định dạng wire

Một khung là:

```text
┌─────────────────────────────────────────┬─────────────────────────────────────────┬─────────────────────────────┐
│ HTTP prefix (60 B)                     │ CSI record header (64 B)               │ CSI I/Q payload             │
│ RawCsiHttpFramePrefix                  │ RawCsiRecordHeaderV8                  │ csi_len_bytes               │
└─────────────────────────────────────────┴─────────────────────────────────────────┴─────────────────────────────┘
```

`frame_len = prefix.header_len + prefix.record_len`, và `record_len` bao gồm
header bản ghi cộng với payload. Bộ giải mã kiểm tra cả hai lớp, mối quan hệ độ dài của chúng,
ID phiên, bộ đếm trình tự, và bốn
kiểm tra chéo bộ đếm prefix↔record.

## API quan trọng

### `FrameStreamDecoder`

Bộ giải mã tăng dần. Sử dụng nó cho tệp, socket, hoặc bất kỳ đầu vào chia nhỏ nào.

| Phương thức | Chữ ký | Ý nghĩa |
|---|---|---|
| `new` | `fn new() -> Self` | Bộ giải mã trống |
| `with_capacity` | `fn with_capacity(bytes: usize) -> Self` | Cấp phát trước bộ đệm |
| `push` | `fn push(&mut self, chunk: &[u8])` | Thêm byte thô; các chunk có thể chia khung ở bất cứ đâu |
| `next_frame` | `fn next_frame(&mut self) -> Result<Option<CsiPacket>, DecodeError>` | Giải mã một khung: `Some` = gói tin, `None` = cần thêm byte, `Err` = dòng không hợp lệ |
| `finish` | `fn finish(&self) -> Result<(), DecodeError>` | Gọi sau khi đầu vào kết thúc; từ chối khung cuối bị cắt xén và byte đuôi |
| `session_id` | `fn session_id(&self) -> Option<[u8; 16]>` | Phiên học từ khung đầu tiên |
| `frames_decoded` | `fn frames_decoded(&self) -> u64` | Khung thành công cho đến nay |
| `bytes_consumed` | `fn bytes_consumed(&self) -> u64` | Offset tuyệt đối của byte đầu tiên chưa được giải mã |
| `buffered_len` | `fn buffered_len(&self) -> usize` | Byte giữ cho khung một phần |

Ví dụ với tệp:

```rust
use std::fs;
use rt_wireless_sensing::decoder::FrameStreamDecoder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read("capture_v8.bin")?;
    let mut decoder = FrameStreamDecoder::new();
    decoder.push(&bytes);

    while let Some(packet) = decoder.next_frame()? {
        println!(
            "seq={} ticks={} amplitude[4]={:.3}",
            packet.frame.stream_sequence,
            packet.csi.device_ticks_us,
            packet.amplitudes()[4],
        );
    }
    decoder.finish()?;
    Ok(())
}
```

Ví dụ kiểu socket:

```rust
let mut decoder = FrameStreamDecoder::new();
while let Some(chunk) = stream.next().await {
    decoder.push(&chunk?);
    while let Some(packet) = decoder.next_frame()? {
        process(packet);
    }
}
decoder.finish()?;
```

### `CsiPacket`

Một bản ghi đã giải mã:

| Trường / phương thức | Kiểu | Ý nghĩa |
|---|---|---|
| `frame` | `HttpFrameHeader` | Các trường prefix vận chuyển 60-byte |
| `csi` | `CsiHeader` | Header bản ghi CSI 21 trường |
| `iq_raw` | `Vec<i8>` | Payload theo thứ tự wire `[Q0, I0, Q1, I1, …]` |
| `subcarriers()` | `usize` | `csi.num_sc` |
| `iq()` | `&[i8]` | Payload xen kẽ thô |
| `iq_pairs()` | `impl Iterator<Item = (f32, f32)>` | Cặp `(I, Q)`, thứ tự wire đã chuyển đổi |
| `amplitudes()` | `Vec<f32>` | `hypot(I, Q)` mỗi subcarrier |
| `phases()` | `Vec<f32>` | `atan2(Q, I)` mỗi subcarrier, radian |
| `valid_wifi_rx_ts_us()` | `Option<u32>` | Timestamp RX driver nếu cờ của nó được đặt |
| `valid_wifi_rx_start_ts_ns()` | `Option<u64>` | Timestamp bắt đầu RX nếu cờ của nó được đặt |
| `valid_noise_floor_dbm()` | `Option<i8>` | Sàn nhiễu, `None` cho giá trị sentinel `-128` |

`CsiHeader` (`packet.csi`) phơi bày `seq_num`, `num_sc`, `csi_len_bytes`,
`device_id`, `device_ticks_us`, `wifi_rx_ts_us`, `channel`, `rssi_dbm`,
`noise_floor_dbm`, `flags`, cộng với trợ giúp tên (`chip_name()`,
`phy_mode_name()`, `ltf_type_name()`, `channel_width_name()`).

### `HttpFrameHeader::parse`

```rust
pub fn parse(bytes: &[u8], base: u64) -> Result<HttpFrameHeader, DecodeError>
```

Chỉ phân tích prefix 60-byte từ `bytes`; `base` là offset tệp/dòng tuyệt đối
được sử dụng trong thông báo lỗi. Kiểm tra độ dài, phiên, và bộ đếm được thực hiện
bởi `FrameStreamDecoder::next_frame`, bởi vì nó cần trạng thái mỗi dòng.

### `DecodeError`

Hai mươi biến thể, mỗi mang theo offset tuyệt đối (ví dụ `Truncated`,
`BadFrameMagic`, `SessionChanged`, `StreamSequenceNotIncreasing`,
`RecordLengthMismatch`, `CounterMismatch`, `TrailingBytes`). Nó triển khai
`Display` và `std::error::Error`.

## Mô-đun độc lập

```bash
# Bảng văn bản cộng với tóm tắt
cargo run --bin decode -- capture_v8.bin

# Chỉ 10 hàng đầu tiên, sau đó tóm tắt
cargo run --bin decode -- capture_v8.bin --limit 10

# JSON Lines (một đối tượng mỗi bản ghi, đối tượng tóm tắt cuối)
cargo run --bin decode -- capture_v8.bin --json

# Chỉ tóm tắt
cargo run --bin decode -- capture_v8.bin --quiet
```

Đầu vào mặc định là `capture_v8.bin`. Tóm tắt báo cáo số khung, byte,
checksum payload FNV-1a 64, ID phiên/thiết bị, khoảng cách trình tự, delta drop và
backpressure, thời lượng, và số bản ghi mỗi giây. Mảng I/Q thô không
được in; sử dụng [`saving`](saving.md) để xuất số.

Tham chiếu xác thực: `capture_v8.bin` phải giải mã thành 691 bản ghi với
checksum payload `0x6446fc78e2c7f3e6` và zero byte đuôi.
