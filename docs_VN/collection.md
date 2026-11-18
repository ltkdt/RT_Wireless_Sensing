# Mô-đun thu thập dòng CSI

Nguồn: [`src/collection.rs`](../src/collection.rs) · Nhị phân độc lập:
[`src/bin/collect.rs`](../src/bin/collect.rs)

Mô-đun này giao tiếp với ESP32 chạy firmware
[ESPectre](https://github.com/francescopace/espectre) và ghi dòng byte CSI thô.
Nó không phân tích các byte; đó là công việc của
[decoder](decoder.md).

## Điều mà một lần thu thập thực hiện

1. `GET /espectre/v1/capabilities` để xác nhận endpoint có thể truy cập.
2. `PATCH /espectre/v1/sensing` với `{"traffic_generator_mode": "external"}`.
3. Gửi các marker lưu lượng UDP 4-byte (`F0 9F 91 BB`) ở PPS được cấu hình.
4. Chờ 200 ms để thiết bị bắt đầu lấy mẫu.
5. `GET /espectre/v1/csi` và sao chép từng chunk phản hồi nguyên văn vào
   tệp đầu ra, ghi đè nếu tệp đã tồn tại.

Tác vụ marker bị hủy khi phát trực tuyến kết thúc hoặc xảy ra lỗi. Cuộc gọi
trả về khi thiết bị đóng dòng (hoặc yêu cầu thất bại), hoặc sớm hơn
khi `max_duration` hết hạn.

## API quan trọng

### `CollectionConfig`

Cài đặt cho một lần chạy. Tất cả các trường đều công khai, vì vậy cú pháp cập nhật cấu trúc là
cách thông thường để thay đổi một giá trị.

| Trường | Kiểu | Mặc định | Ý nghĩa |
|---|---|---|---|
| `esp32_ip` | `String` | `"10.178.22.9"` | Địa chỉ ESP32 |
| `api_port` | `u16` | `62587` | Cổng API HTTP ESPectre |
| `marker_port` | `u16` | `5555` | Cổng marker UDP |
| `pps` | `u32` | `20` | Gói marker mỗi giây; phải khác không |
| `origin` | `String` | `"https://test.espectre.dev"` | Tiêu đề HTTP `Origin` |
| `output` | `PathBuf` | `"capture_v8.bin"` | Đích ghi thô |
| `startup_delay` | `Duration` | `200 ms` | Chờ giữa bật cảm biến và `/csi` |
| `max_duration` | `Option<Duration>` | `None` | Dừng ghi sau khoảng thời gian này; `None` ghi cho đến khi luồng kết thúc |
| `verbose` | `bool` | `true` | In tiến độ cho mọi chunk nhận được lên stderr |

Hai địa chỉ IP đã ghi của ESP32:
- 10.178.22.9
- 10.178.22.62

Trợ giúp:

- `CollectionConfig::default()` — các giá trị trên.
- `CollectionConfig::with_output(path)` — mặc định với đầu ra khác.
- `config.base_url() -> String` — `http://<ip>:<port>/espectre/v1`.
- `config.marker_period() -> Duration` — `1000 / pps` ms, giới hạn ở ít nhất
  1 ms.
- `config.marker_target() -> Result<SocketAddr, CollectionError>` — đích UDP.

### `collect`

```rust
pub async fn collect(
    config: &CollectionConfig,
) -> Result<CollectionSummary, CollectionError>
```

Chạy toàn bộ trình tự ở trên. Nó là `async` và phải được await trong runtime Tokio.
Bỏ future hủy yêu cầu HTTP và tác vụ marker.

Ví dụ:

```rust
use rt_wireless_sensing::collection::{CollectionConfig, collect};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = CollectionConfig {
        esp32_ip: "192.168.1.50".to_string(),
        output: "capture.bin".into(),
        ..CollectionConfig::default()
    };

    let summary = collect(&config).await?;
    println!("{} bytes -> {}", summary.bytes_written, summary.output.display());
    Ok(())
}
```

### `CollectionSummary`

Trả về bởi `collect`:

| Trường | Kiểu | Ý nghĩa |
|---|---|---|
| `output` | `PathBuf` | Tệp đã được ghi |
| `bytes_written` | `u64` | Tổng byte body được sao chép |
| `duration` | `Duration` | Thời lượng theo đồng hồ của giai đoạn phát trực tuyến |
| `configured_pps` | `u32` | Tốc độ marker được sử dụng |
| `capabilities` | `String` | JSON thô từ `GET /capabilities` |
| `sensing` | `String` | JSON thô từ `PATCH /sensing` |

### `CollectionError`

| Biên thể | Nguyên nhân |
|---|---|
| `Http(reqwest::Error)` | Yêu cầu thất bại, trạng thái non-2xx, hoặc đọc body thất bại |
| `Io(std::io::Error)` | Không thể tạo/ghi/xóa bộ đệm tệp đầu ra |
| `InvalidEndpoint { endpoint, source }` | Địa chỉ ESP32 không phải là địa chỉ socket hợp lệ |
| `InvalidPps` | `pps == 0` |

Triển khai `Display` và `std::error::Error`; `?` hoạt động ở đâu
`CollectionError: Into<E>`.

## Mô-đun độc lập

```bash
# Trợ giúp và tất cả tùy chọn
cargo run --bin collect -- --help

# Thiết bị mặc định, đầu ra mặc định capture_v8.bin
cargo run --bin collect

# Thiết bị và đầu ra rõ ràng
cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin

# Marker nhanh hơn, không tiến độ theo chunk
cargo run --bin collect -- --pps 50 --quiet

# Ghi trong 20 giây (đơn vị mặc định là giây; myös ms/s/m/h hoạt động)
cargo run --bin collect -- --duration 20
```

Tùy chọn: `--ip`, `--api-port`, `--marker-port`, `--pps`, `--origin`,
`--output`, `--duration`, `--quiet`, `-h/--help`.

Nhị phân mặc định (`cargo run` hoặc
`cargo run --bin rt-wireless-sensing`) gọi `collect` với
`CollectionConfig::default()`, vì vậy nó không cần đối số.

## Yêu cầu

- ESP32 phải có thể truy cập tại `esp32_ip:api_port` và đang chạy ESPectre.
- Marker UDP được gửi tới `esp32_ip:marker_port`; thiết bị phải ở
  chế độ lưu lượng bên ngoài (mô-đun bật nó).
- Thư mục đầu ra phải đã tồn tại.

Tệp `.bin` kết quả là đầu vào của [`saving`](saving.md).
