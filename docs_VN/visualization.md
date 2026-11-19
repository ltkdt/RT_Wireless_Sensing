# Mô-đun hiển thị CSI

Nguồn: [`src/visualization.rs`](../src/visualization.rs) · Nhị phân độc lập:
[`src/bin/visualize.rs`](../src/bin/visualize.rs) · Tài sản web:
[`web/`](../web)

Mô-đun này cung cấp bảng điều khiển đồ họa khoa học để hiển thị dữ liệu CSI Wi-Fi đã giải mã.
Nó lấy bản ghi CSI từ [decoder](decoder.md) và hiển thị chúng dưới dạng heatmap biên độ 2D bằng HTML Canvas 2D API,
phù hợp với yêu cầu trong `plan.md`.

```text
capture_v8.bin ──decode──► VisualizationData ──export──► capture_v8_heatmap.html
                             ├─ metadata                  (bảng điều khiển độc lập đơn tệp)
                             ├─ timestamps (giây)       HOẶC
                             ├─ amplitudes [N, 64]   ──serve──► http://127.0.0.1:8080/
                             └─ classification (tùy chọn)
```

## Tính năng

### 1. Heatmap biên độ CSI 2D
* **Renderer**: HTML5 Canvas 2D API gốc với hỗ trợ High-DPI (Retina).
* **Trục X**: Thời gian đo lường tính bằng giây. Bản ghi tĩnh bắt đầu từ `0.0`; cửa sổ phát trực tuyến giữ nhãn tuyệt đối (ví dụ, `2–32 giây` sau khi cửa sổ 30 giây trượt), bảo toàn thứ tự thời gian.
* **Trục Y**: Chỉ số subcarrier ($0 \dots 63$). Subcarrier 0 được render ở dưới cùng và subcarrier 63 ở trên cùng.
* **Bản đồ màu**: Bản đồ màu khoa học đồng nhất về mặt cảm nhận (**Viridis** [mặc định], **Plasma**, và **Jet**).
* **Colorbar**: Gradient dọc với vạch chia biên độ số và đơn vị (`Biên độ (a.u.)`).
* **Tương tác**: Di chuột động với đường hướng dẫn chéo, đọc tọa độ (Thời gian, Subcarrier, Biên độ, Khung), và popup tooltip.
* **Độ chiếm chỗ khi nhấp**: Nhấp vào điểm heatmap để tính toán độ chiếm chỗ của giây chứa điểm đó. Xem [Độ chiếm chỗ](#4-nhấp-để-tính-toán-độ-chiếm-chỗ).

### 2. Thông tin đo lường
Hiển thị thống kê ghi quan trọng cùng với heatmap:
* **Thời gian bắt đầu**: Bắt đầu tương đối và đồng hồ thiết bị đơn điệu (`device_ticks_us`).
* **Thời lượng**: Tổng thời gian ghi đã trôi qua tính bằng giây.
* **Khung đã giải mã**: Tổng số bản ghi CSI hợp lệ ($N$).
* **Tốc độ hiệu quả**: Gói mỗi giây (PPS) tính từ delta đồng hồ thiết bị.

### 3. Khu vực kết quả phân loại
Cung cấp thẻ chuyên dụng hiển thị lớp hoạt động hoặc cử chỉ được dự đoán (ví dụ "Chế độ chờ / Không", "Đi bộ", "Vẫy tay") với mức độ tin cậy khi kết quả phân loại được kết nối.

### 4. Nhấp để tính toán độ chiếm chỗ
Nhấp vào điểm heatmap chọn giây chứa điểm đó (ví dụ,
`24.3 giây → [24, 25)`). Trình hiển thị chia giây đó thành `N` khe bằng nhau,
trong đó `N` là PPS danh định, và báo cáo:

```text
độ chiếm chỗ = (số khe chứa ít nhất một phép đo) / N
```

Kết quả được hiển thị trong hàng hover dưới dạng **Độ chiếm chỗ (nhấp)**, ví dụ
`87.0% (87/100 @ 24s)`. Lệnh phát trực tuyến đặt `N` từ `--pps`; bản ghi tĩnh
sử dụng PPS hiệu quả được đo. Giá trị được tính toán lại trên mỗi lần làm mới dữ liệu trực tiếp
và biến mất khi giây rời khỏi cửa sổ 30 giây.

Đây là phép tính phía trình duyệt trên `timestamps` hiển thị; không có điểm cuối bổ sung nào liên quan.

### 5. Chế độ chẩn đoán (phân tích thiếu hụt PPS)

Một canvas riêng nằm sau công tắc ở cuối trang, độc lập với heatmap biên độ
(heatmap không bị ảnh hưởng khi công tắc tắt). Nó trả lời một câu hỏi: *vì sao
mỗi giây chỉ thu được ít mẫu CSI hơn PPS đã cấu hình?*

Nhấp vào một điểm trên heatmap để chọn giây của điểm đó, rồi bật **Chế độ chẩn
đoán**. Canvas vẽ ma trận `subcarrier × khe` của giây đó (`khe` = PPS danh định
được làm tròn một lần trong Rust); các cột trống được gạch chéo. Di chuột lên một
ô chỉ hiện một dòng định danh (`Slot 12/99 · SC 31 — filled (frame #431)`); nhấp
vào ô sẽ đổ dữ liệu vào hộp **Chi tiết khe thời gian** bên dưới canvas, còn hộp
**Thống kê** hiển thị các con số của giây đó.

Mọi giá trị đều được tính trong Rust từ chính tệp `.bin` — tiền tố HTTP 60 byte
và header bản ghi CSI 64 byte — rồi gửi trong trường `diagnostics` của payload,
nên có thể xuất cùng các con số đó bằng `--json` để phân tích ngoại tuyến.

#### Payload

| Trường | Ý nghĩa |
|---|---|
| `slots_per_second` | Số cột của lưới (PPS danh định làm tròn một lần) |
| `origin_ticks` | `device_ticks_us` ứng với mốc thời gian `0.0` |
| `capture_end` | `true` với tệp ngoại tuyến, `false` với biên cửa sổ trực tiếp |
| `records[]` | Một dòng cho mỗi bản ghi nhận được: khe, độ lệch khe, Δstream, Δdrop, Δbackpressure, Δticks, Δwifi_rx, RSSI/noise/SNR, flags, kênh/PHY/độ rộng |
| `empty_slots[]` | Một dòng cho mỗi khe không có bản ghi: nguyên nhân, số khung bị mất, bản ghi biên |
| `seconds[]` | Thống kê từng giây: số lượng, khoảng Δtick, cụm trống, độ lệch khe, bộ đếm theo tầng, chất lượng vô tuyến, bất thường |

#### Ngữ nghĩa khe

`khe = floor((device_ticks_us − đầu_giây) × khe_mỗi_giây / 1_000_000)`, tính bằng
số nguyên trên tick thiết bị — đúng dạng của công thức độ chiếm chỗ phía trình
duyệt `floor((t − đầu_giây) × khe + 1e-6)`. Một bản ghi thuộc khe nửa mở chứa
`device_ticks_us` của nó; `slot_offset_us` là khoảng cách có dấu tới mốc bắt đầu
khe lý thuyết (dương = trễ).

#### Tầng nào gây lỗi?

Mọi bộ đếm đều được ghi trên thiết bị RX, nhưng mỗi bộ đếm chỉ về một tầng khác
nhau:

| Dấu hiệu | Tầng | Kết luận | Hành động tiếp theo |
|---|---|---|---|
| Khe trống, `stream_sequence` có khoảng nhảy, Δ`raw_drop_total` > 0 | **RX (thiết bị)** — bản ghi không được gửi (hàng đợi đầy / không hợp lệ / gửi lô lỗi) | Thiết bị bỏ do hàng đợi đầy | Giảm PPS, tăng kích thước lô, kiểm tra CPU firmware |
| Khe trống, có khoảng nhảy, Δ`raw_send_backpressure_total` > 0 | **Desktop** — client không nhận lô | Desktop đọc quá chậm | Tăng bộ đệm client, giảm nhịp làm mới 500 ms |
| Khe trống, bộ đếm liên tục, RSSI lân cận thấp | **TX/airtime** (AP → thiết bị) | Mất trên không (airtime) | Đổi kênh, giảm lưu lượng khác, giảm PPS |
| Khe trống, bộ đếm liên tục, RSSI bình thường | **Generator (desktop)** | Marker không được gửi / firmware không tạo CSI | Đối chiếu với log marker đã gửi |
| Có mẫu, độ lệch khe lớn, độ lệch chuẩn khoảng cao | **Generator (desktop)** jitter | Jitter thời gian | Đo lại generator |

`raw_drop_total` là mất phía **RX** (bản ghi chưa từng rời thiết bị);
`raw_send_backpressure_total` là lỗi nhận phía **desktop**. Cùng triệu chứng
(bản ghi thiếu trong tệp) nhưng ngược phía.

"RSSI thấp" nghĩa là bằng hoặc dưới `LOW_RSSI_THRESHOLD_DBM` (−70 dBm) ở một
trong hai bản ghi biên. Khoảng nhảy mà các bộ đếm không giải thích được sẽ được
báo là `ambiguous` thay vì đoán.

#### Hạn chế

- Không có định danh marker: bản ghi không lưu marker UDP nào đã tạo ra nó, nên
  không thể chứng minh "marker không được gửi" chỉ từ tệp.
- Đường truyền là HTTP trên TCP nên không thể mất trên đường: khoảng nhảy
  `stream_sequence` là mất phía thiết bị, không phải mất mạng.
- `wifi_rx_ts_us` thuộc miền đồng hồ khác (và 32-bit), nên chỉ Δ tương đối có
  nghĩa; `seq_num` bão hòa ở `u32::MAX`.
- Giây cuối của bản ghi, và cả hai biên của cửa sổ trực tiếp, là giây một phần:
  `seconds[].partial` được bật và tỉ lệ mất chỉ mô tả phần đã phủ.
- Payload không nhỏ: bản ghi 34 giây với 100 khe mang khoảng 1 MB dữ liệu chẩn
  đoán (bản ghi + khe trống + thống kê) bên cạnh ma trận biên độ.

#### `--pps` cho tệp ngoại tuyến

Không có `--pps`, tệp ngoại tuyến dùng tốc độ hiệu quả đo được, làm lưới co lại
đúng ở những giây bị thiếu mẫu. Hãy truyền tốc độ đã cấu hình để giữ lưới đúng
kích thước:

```bash
cargo run --bin visualize -- capture_v8.bin --pps 100
```

## CLI độc lập

```bash
# Giải mã capture_v8.bin và xuất bảng điều khiển HTML độc lập
cargo run --bin visualize -- capture_v8.bin

# Chỉ định đường dẫn đầu ra HTML tùy chỉnh và mở tự động trong trình duyệt mặc định
cargo run --bin visualize -- capture_v8.bin --output dashboard.html --open

# Xuất dữ liệu hiển thị JSON
cargo run --bin visualize -- capture_v8.bin --json csi_data.json

# Khởi động máy chủ web nhúng không đồng bộ
cargo run --bin visualize -- capture_v8.bin --serve --port 8080
```

### Tùy chọn CLI

| Tùy chọn | Mô tả |
|---|---|
| `FILE` | Tệp ghi thô được viết bởi `collect` (mặc định: `capture_v8.bin`) |
| `-o, --output FILE` | Đường dẫn đích cho bảng điều khiển HTML độc lập (mặc định: `<stem>_heatmap.html`) |
| `--json FILE` | Đường dẫn xuất dữ liệu hiển thị JSON |
| `--pps PPS` | Tốc độ gói danh định cho lưới khe chẩn đoán (mặc định: tốc độ hiệu quả đo được) |
| `--serve` | Khởi động máy chủ web nhúng không đồng bộ |
| `--port PORT` | Cổng cho máy chủ web (mặc định: `8080`) |
| `--open` | Mở tệp HTML đã tạo hoặc URL máy chủ web trong trình duyệt hệ thống mặc định |
| `-q, --quiet` | Ẩn đầu ra văn bản tóm tắt |
| `-h, --help` | Hiển thị thông tin cách sử dụng |

## API thư viện

### Chuẩn bị dữ liệu hiển thị

```rust
use std::path::Path;
use rt_wireless_sensing::visualization::VisualizationData;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Giải mã và chuẩn bị dữ liệu từ tệp nhị phân
    let data = VisualizationData::from_file(Path::new("capture_v8.bin"))?;

    println!("Đã giải mã {} khung qua {} giây", 
        data.metadata.total_frames, 
        data.metadata.duration_seconds
    );

    // Xuất gói HTML độc lập (nhúng HTML, CSS, JS, và dữ liệu JSON)
    data.export_html(Path::new("csi_dashboard.html"))?;

    Ok(())
}
```

Truyền tốc độ gói đã cấu hình khi biết bản ghi bị thiếu mẫu, để lưới khe chẩn
đoán giữ đúng kích thước mong muốn:

```rust
use rt_wireless_sensing::visualization::{
    VisualizationData, VisualizationOptions, prepare_visualization_data_with_options,
};

// Tệp ngoại tuyến với PPS danh định tường minh.
let data = VisualizationData::from_file_with_pps(Path::new("capture_v8.bin"), Some(100.0))?;

// Hoặc từ các gói đã giải mã, kiểm soát đầy đủ dấu vết chẩn đoán.
let packets = rt_wireless_sensing::visualization::decode_packets(&bytes)?;
let data = prepare_visualization_data_with_options(
    &packets,
    Some("capture_v8.bin"),
    packets[0].csi.device_ticks_us,
    VisualizationOptions { nominal_pps: Some(100.0), capture_end: true },
)?;
```

### Đính kèm kết quả phân loại

```rust
let data = VisualizationData::from_file(Path::new("capture_v8.bin"))?
    .with_classification(
        Some("WaveHand".to_string()),
        Some(0.95),
        Some("Model v1.0 inference".to_string()),
    );

data.export_html(Path::new("predicted.html"))?;
```

### Chạy máy chủ web nhúng

```rust
use rt_wireless_sensing::visualization::{VisualizationData, serve_dashboard};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = VisualizationData::from_file(std::path::Path::new("capture_v8.bin"))?;
    serve_dashboard(data, "127.0.0.1:8080").await?;
    Ok(())
}
```

### Chạy bảng điều khiển trực tiếp

`serve_dashboard` phục vụ một bộ dữ liệu tĩnh. Để cập nhật trực tiếp, sử dụng
`serve_dashboard_live` (hoặc `serve_dashboard_live_on` với `TcpListener` đã ràng buộc
trước) và cập nhật [`LiveDashboardSnapshot`] chia sẻ:

```rust
use std::sync::{Arc, RwLock};
use rt_wireless_sensing::visualization::{
    LiveDashboardSnapshot, serve_dashboard_live,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(RwLock::new(LiveDashboardSnapshot::Waiting {
        elapsed_seconds: 0.0,
        warmup_seconds: 3.0,
    }));

    // Nhà sản xuất có thể thay thế snapshot mỗi 500 ms:
    // *state.write().unwrap() = LiveDashboardSnapshot::Ready {
    //     data: Box::new(data),
    // };

    serve_dashboard_live(Arc::clone(&state), "127.0.0.1:8080").await?;
    Ok(())
}
```

`LiveDashboardSnapshot` có hai biến thể:

- `Waiting { elapsed_seconds, warmup_seconds }` — trình duyệt giữ overlay chờ
  và không vẽ heatmap.
- `Ready { data: Box<VisualizationData> }` — trình duyệt thay thế heatmap.

Mô-đun [`streaming`](streaming.md) sử dụng API này để xuất bản cửa sổ trượt 30 giây
mỗi 500 ms. Mã trình duyệt của nó thăm dò `/api/data` mỗi 500 ms và cũng
chấp nhận hình dạng `VisualizationData` tĩnh được phát bởi `serve_dashboard`.

## Điểm cuối máy chủ web

Khi chạy `serve_dashboard`, `serve_dashboard_live`, hoặc `visualize --serve`:
* `GET /` — Phục vụ trang HTML bảng điều khiển.
* `GET /api/data` — Trả về payload hiển thị hiện tại dưới dạng JSON.
* `GET /style.css` — Phục vụ bảng định kiểu.
* `GET /visualizer.js` — Phục vụ kịch bản hiển thị Canvas 2D.

Máy chủ tĩnh trả về [`VisualizationData`](#visualizationdata) trực tiếp.
Máy chủ trực tiếp trả về snapshot có thẻ:

```json
{"status":"waiting","elapsed_seconds":1.5,"warmup_seconds":3.0}
{"status":"ready","data":{"metadata":{...},"timestamps":[...],"amplitudes":[[...]],"classification":null}}
```

Cả hai máy chủ đều gửi `Cache-Control: no-store` cho `/api/data`.

## Phát trực tuyến theo thời gian thực

Để thu thập và hiển thị cùng lúc, sử dụng
lệnh [`streaming`](streaming.md) thay vì chạy `visualize --serve` trên
một tệp vẫn đang được ghi:

```bash
cargo run --bin streaming -- --ip 10.178.22.9 --output capture_v8.bin --pps 100 --port 8080
```

Trường metadata `nominal_pps` là số lượng khe được sử dụng bởi đầu ra độ chiếm chỗ.
Mô-đun phát trực tuyến đặt nó thành `--pps` được cấu hình; hiển thị dựa trên tệp
mặc định nó thành `effective_pps` được đo.
