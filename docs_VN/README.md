# Tài liệu Mô-đun

Gói Rust được chia thành các mô-đun đường ống cộng với schema bản ghi chung.
Mỗi mô-đun có tệp riêng trong [`src/`](../src) và một nhị phân độc lập để
sử dụng dòng lệnh.

| Mô-đun | Nguồn | Mục đích | Lệnh độc lập | Tài liệu |
|---|---|---|---|---|
| Thu thập | `src/collection.rs` | Truyền dòng CSI thô từ ESPectre vào tệp `.bin` | `cargo run --bin collect -- --help` | [collection.md](collection.md) |
| Giải mã | `src/decoder.rs` | Phân tích và xác thực khung CSI V8 | `cargo run --bin decode -- capture_v8.bin` | [decoder.md](decoder.md) |
| Lưu trữ | `src/saving.rs` | Xuất timestamp/biên độ/pha ra `.npz` | `cargo run --bin save -- capture_v8.bin` | [saving.md](saving.md) |
| DSP | `src/dsp/` | Xử lý tín hiệu đồng bộ trên CSI đã giải mã (`CsiSignal`): chuẩn hóa, resample, lọc Hampel/biquad, FFT/STFT | — (chỉ thư viện) | [dsp.md](dsp.md) |
| Hiển thị | `src/visualization.rs` | Render bảng điều khiển heatmap biên độ CSI 2D | `cargo run --bin visualize -- capture_v8.bin` | [visualization.md](visualization.md) |
| Phát trực tuyến | `src/streaming.rs` | Thu thập, giải mã trực tiếp và phục vụ heatmap trượt 30 giây | `cargo run --bin streaming -- --help` | [streaming.md](streaming.md) |
| Schema | `src/csi_header.rs` | Các trường và hằng số header bản ghi V8 64-byte | — | [decoder.md](decoder.md) |

## Luồng điển hình

```bash
# 1. Ghi khung thô (mặc định main làm điều tương tự mà không có đối số)
cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin

# 2. Kiểm tra dòng (tùy chọn)
cargo run --bin decode -- capture_v8.bin --quiet

# 3. Xuất mảng số để phân tích
cargo run --bin save -- capture_v8.bin

# 4. Hiển thị heatmap biên độ 2D
cargo run --bin visualize -- capture_v8.bin --open

# 5. Hoặc thu thập và hiển thị trực tiếp cùng lúc
cargo run --bin streaming -- --ip 10.178.22.9 --output capture_v8.bin --pps 100 --port 8080
```

Nhị phân mặc định (`cargo run` / `cargo run --bin rt-wireless-sensing`) gọi
mô-đun thu thập với `CollectionConfig::default()`.

## Sử dụng thư viện

Thêm các mô-đun của gói vào chương trình Rust khác:

```rust
use rt_wireless_sensing::{
    collection::{CollectionConfig, collect},
    decoder::FrameStreamDecoder,
    saving::save_capture,
    streaming::{StreamingConfig, run},
    visualization::VisualizationData,
};
```

Tất cả các mục công khai được tài liệu hóa với `cargo doc --open`.
