# Mô-đun phát trực tuyến

Nguồn: [`src/streaming.rs`](../src/streaming.rs) · Nhị phân độc lập:
[`src/bin/streaming.rs`](../src/bin/streaming.rs) · Liên quan:
[collection](collection.md), [decoder](decoder.md), [visualization](visualization.md)

Mô-đun này ghi CSI và phục vụ heatmap biên độ trực tiếp cùng lúc.
Nó tổ hợp các mô-đun thu thập, bộ giải mã, và hiển thị; nó không
sao chép mã mạng, phân tích cú pháp, hoặc render của chúng.

```text
ESPectre /csi ──► collection ──► raw .bin
                       │
                       └── chunks ──► decoder ──► 30 giây cửa sổ gói tin
                                                   │ mỗi 500 ms
                                                   ▼
                                         bảng điều khiển trực tiếp :8080
```

## Lệnh

```bash
cargo run --bin streaming -- \
  --ip 10.178.22.9 \
  --output capture_v8.bin \
  --pps 100 \
  --port 8080
```

Mở `http://127.0.0.1:8080` trong trình duyệt. Máy chủ bắt đầu trước khi thu thập,
vì vậy trang có sẵn ngay lập tức; bản ghi thô được ghi vào
`--output` cùng lúc.

### Tùy chọn CLI

| Tùy chọn | Mặc định | Ý nghĩa |
|---|---|---|
| `--ip ADDRESS` | `10.178.22.9` | Địa chỉ IPv4 ESP32 |
| `--api-port PORT` | `62587` | Cổng API HTTP ESPectre |
| `--marker-port PORT` | `5555` | Cổng marker UDP |
| `--pps N` | `20` | Tốc độ marker; cũng là số lượng khe chiếm chỗ |
| `--origin URL` | `https://test.espectre.dev` | Tiêu đề HTTP `Origin` |
| `--output FILE` | `capture_v8.bin` | Tệp ghi thô |
| `--port PORT` | `8080` | Cổng bảng điều khiển trên `127.0.0.1` |
| `--verbose` | tắt | In tiến độ thu thập theo chunk |
| `-h, --help` | — | Hiển thị cách sử dụng |

## Hành vi định thời

- **0–3 giây:** bảng điều khiển chỉ hiển thị thông báo chờ ("Đang chờ dữ liệu CSI… x / 3 giây"). Không vẽ heatmap.
- **Từ 3 giây trở đi:** heatmap đầu tiên được xuất bản. Một snapshot được xuất bản
  mỗi **500 ms**.
- **Cửa sổ trượt:** chỉ giữ **30 giây** thời gian thiết bị gần nhất.
  Ở thời điểm chạy 32 giây, dữ liệu từ 0–2 giây bị xóa.
- **Trục tuyệt đối:** trục x hiển thị thời gian đã trôi qua kể từ khi bắt đầu
  ghi, vì vậy cửa sổ ở 32 giây được dán nhãn `2–32`, không phải `0–30`.
- Snapshot đầu tiên không bao giờ được xuất bản trước 3 giây, ngay cả khi bản ghi đến
  sớm hơn. Nếu không có bản ghi nào đến lúc 3 giây, thông báo chờ vẫn hiển thị cho đến bản ghi đầu tiên.

Cửa sổ được cắt bởi `device_ticks_us` (đồng hồ mẫu đơn điệu của thiết bị),
không phải bởi thời gian tường đến. Do đó, mạng chậm hoặc gián đoạn không
làm méo trục thời gian.

## Độ chiếm chỗ (nhấp để tính toán)

Nhấp bất cứ đâu trên heatmap. Lệnh lấy giây chứa điểm được nhấp (`24.3 giây → [24, 25)`), chia giây đó thành **N khe bằng nhau**
trong đó `N` là `--pps` được cấu hình, và đếm có bao nhiêu khe chứa ít nhất
một phép đo:

```text
dộ chiếm chỗ = N_đã_điền / N
```

Ví dụ: `--pps 100`, 87 khe chứa gói tin → `87.0% (87/100 @ 24s)`.
Kết quả xuất hiện trong hàng hover dưới dạng **Độ chiếm chỗ (nhấp)**. Nó
được tính toán lại khi cửa sổ trực tiếp làm mới và biến mất khi giây đó
rời khỏi cửa sổ 30 giây.

Số lượng khe đến từ `metadata.nominal_pps` (được đặt thành `--pps` bởi
mô-đun này). Bản ghi tĩnh sử dụng PPS hiệu quả được đo.

## API thư viện

### `StreamingConfig`

```rust
pub struct StreamingConfig {
    pub collection: CollectionConfig,
    pub port: u16,                    // mặc định 8080
    pub warmup: Duration,             // mặc định 3 giây
    pub update_interval: Duration,    // mặc định 500 ms
    pub window: Duration,             // mặc định 30 giây
}
```

- `StreamingConfig::default()` sử dụng địa chỉ ESP32 tiêu chuẩn, 20 PPS, và
  `capture_v8.bin`. Ghi log thu thập theo chunk tắt theo mặc định.
- `config.bind_addr()` trả về `127.0.0.1:<port>`.

### `run`

```rust
pub async fn run(
    config: &StreamingConfig,
) -> Result<StreamingSummary, StreamingError>
```

Bắt đầu bảng điều khiển, bắt đầu thu thập, giải mã trực tiếp, và chạy cho đến Ctrl+C hoặc
dòng thu thập kết thúc.

```rust
use rt_wireless_sensing::streaming::{StreamingConfig, run};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = StreamingConfig::default();
    config.collection.esp32_ip = "10.178.22.9".to_string();
    config.collection.pps = 100;
    config.port = 8080;

    let summary = run(&config).await?;
    println!(
        "{} records, {} bytes -> {}",
        summary.records_decoded,
        summary.bytes_written,
        summary.output.display()
    );
    Ok(())
}
```

### `StreamingSummary`

| Trường | Ý nghĩa |
|---|---|
| `output` | `.bin` thô được ghi trong quá trình chạy |
| `records_decoded` | Tổng số bản ghi đã giải mã, bao gồm những bản ghi đã ngoài cửa sổ |
| `window_records` | Bản ghi trong cửa sổ cuối cùng được xuất bản |
| `bytes_written` | Kích thước bản ghi thô khi phát trực tuyến dừng |
| `duration` | Thời lượng theo đồng hồ của quá trình chạy |

### `StreamingError`

Bao bọc `CollectionError`, `DecodeError`, `VisualizationError`, lỗi I/O,
lỗi join tác vụ nền, và cấu hình không hợp lệ. Triển khai `Display`
và `std::error::Error`.

## Điểm cuối bảng điều khiển

Máy chủ trực tiếp sử dụng tài sản và điểm cuối của mô-đun hiển thị:

- `GET /` — HTML bảng điều khiển
- `GET /api/data` — snapshot hiện tại
- `GET /style.css` — bảng định kiểu
- `GET /visualizer.js` — JavaScript

`/api/data` là:

```json
{"status":"waiting","elapsed_seconds":1.5,"warmup_seconds":3.0}
```

hoặc:

```json
{"status":"ready","data":{"metadata":{...},"timestamps":[...],"amplitudes":[[...]],"classification":null}}
```

Trình duyệt thăm dò mỗi 500 ms với `Cache-Control: no-store`.

## Ghi chú

- Nhấn **Ctrl+C** để dừng thu thập, marker UDP, và máy chủ. Bản ghi thô đã ghi
  cho đến lúc đó vẫn còn trên đĩa.
- Khung cuối bị cắt xén do Ctrl+C bị bỏ qua bởi view trực tiếp; `.bin`
  vẫn chứa các byte đã được ghi.
- Mô-đun này ghi `.bin` thô, không phải `.npz`. Sử dụng [`saving`](saving.md) trên
  bản ghi sau đó nếu cần mảng số.
- Yêu cầu ESP32 có thể truy cập chạy ESPectre, giống như `collect`.
- Chỉ `127.0.0.1` được ràng buộc; sử dụng SSH tunnel hoặc proxy đảo ngược để xem
  từ máy khác.
