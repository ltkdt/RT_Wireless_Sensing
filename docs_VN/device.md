# Mô-đun device

Nguồn: [`src/device.rs`](../src/device.rs) · Nhị phân độc lập:
[`src/bin/device.rs`](../src/bin/device.rs)

Mô-đun này quản lý **trạng thái của thiết bị RX**: ai tạo ra lưu lượng mà ESP32
đang thu, và tốc độ đích là bao nhiêu. Đây là mô-đun duy nhất nói chuyện với
thiết bị về chế độ của nó, và là nơi duy nhất trong gói *ghi* vào thiết bị.

```text
GET  /espectre/v1/sensing ──parse_status──▶ DeviceStatus ─┬─ is_internal()
                                                          ├─ traffic_generator()
                                                          └─ csi_target_pps
PATCH /espectre/v1/sensing ◀── set_traffic_generator_mode ── (chỉ CLI)
```

## Hai chế độ, một ý nghĩa

| `traffic_generator_mode` của thiết bị | [`TrafficGenerator`] | Ai gửi gói | Cờ thu thập |
|---|---|---|---|
| `external` | `External` | Gói này, dưới dạng marker UDP 4-byte | `collect --pps N` |
| `ping`, `dns`, `dns_tcp`, `wifi_raw`, mọi giá trị tương lai | `Internal` | Chính thiết bị | `collect --internal` |

`is_internal()` coi **mọi** giá trị khác `external` là internal, kể cả những chế
độ gói này không biết, vì tất cả đều có nghĩa "thiết bị gửi gói". Nhờ vậy firmware
thêm generator mới vẫn dùng được thay vì bị hiểu nhầm là external.

## Đọc

### `read(endpoint) -> Option<DeviceStatus>`

Một `GET /sensing`. Trả `None` khi lỗi truyền tải, trạng thái non-2xx, hoặc body
không parse được — nơi gọi coi `None` là **chưa biết**, không phải thất bại, nên
thiết bị không truy cập được chỉ gây cảnh báo và vẫn có thể thu rồi phân tích
ngoại tuyến.

### `parse_status(body) -> Option<DeviceStatus>`

Tách khỏi `read` để kiểm thử phần parse mà không cần mạng. Các trường lạ được bỏ
qua (theo yêu cầu của API ESPectre với client); chỉ `traffic_generator_mode` và
`csi_target_pps` được đọc. Trả `None` khi body không phải object JSON.

### `DeviceStatus`

| Trường | Kiểu | Ý nghĩa |
|---|---|---|
| `traffic_generator_mode` | `Option<String>` | Chế độ firmware báo, nguyên văn |
| `csi_target_pps` | `Option<u32>` | Tốc độ đích của thiết bị; `None` khi thiếu, không phải số nguyên, hoặc không dương |

`traffic_generator()` ánh xạ chuỗi thô sang [`TrafficGenerator`] và là `None` khi
firmware không báo chế độ.

## Kiểm tra: `require_mode`

```rust
pub fn require_mode(
    status: Option<&DeviceStatus>,
    requested: TrafficGenerator,
) -> Result<(), DeviceError>
```

| Trạng thái thiết bị | Yêu cầu | Kết quả | Lý do |
|---|---|---|---|
| không đọc được (`None`) | bất kỳ | `Ok(())` | Chưa biết không phải sai; lỗi mạng tạm thời không được thành lỗi chí mạng |
| không có trường `traffic_generator_mode` | bất kỳ | `Ok(())` | Firmware cũ; không có gì mâu thuẫn |
| `external` | `External` | `Ok(())` | Khớp |
| `ping` / `dns` / … | `Internal` | `Ok(())` | Khớp |
| `external` | `Internal` | `Err(Mismatch)` | Thiết bị sẽ không tạo ra lưu lượng đang được thu |
| `ping` / `dns` / … | `External` | `Err(Mismatch)` | Thêm một nguồn kích thích thứ hai không được đếm, chồng lên marker |

Nơi gọi phải coi mismatch là lỗi chí mạng và dừng **trước** khi mở dòng CSI, để
không tệp capture nào được tạo cho một lần chạy lẽ ra không thu được gì.
[`Mismatch::remedy`] in đúng lệnh `device --mode …` cần chạy, nên thông báo và
cách khắc phục không thể lệch nhau.

## Ghi: `set_traffic_generator_mode`

```rust
pub async fn set_traffic_generator_mode(
    endpoint: &DeviceEndpoint,
    mode: &str,
) -> Result<DeviceStatus, DeviceError>
```

Lệnh ghi **duy nhất** trong gói; không phần nào của đường thu thập gọi nó. Nó:

1. từ chối chế độ không nằm trong [`TRAFFIC_GENERATOR_MODES`] ngay *tại chỗ*, nên
   lỗi gõ không bao giờ tới được NVS của thiết bị;
2. gửi `PATCH /sensing` với `{"traffic_generator_mode": mode}`;
3. đọc lại thiết bị thay vì tin phản hồi — trả lời PATCH chỉ là xác nhận, không
   phải bằng chứng. Đọc lại thất bại là `Unconfirmed` (kết quả chưa biết), không
   bao giờ là "ghi không có tác dụng".

## Lỗi

| Biến thể | Nguyên nhân |
|---|---|
| `Http(reqwest::Error)` | Yêu cầu thất bại hoặc trả trạng thái không thành công |
| `Mismatch(Box<Mismatch>)` | Chế độ thiết bị mâu thuẫn với chế độ được yêu cầu |
| `UnknownMode { mode }` | Chế độ yêu cầu không phải chế độ thiết bị chấp nhận |
| `Unconfirmed { mode }` | Lệnh ghi được chấp nhận nhưng không đọc lại được thiết bị |

Triển khai `Display` và `std::error::Error`; `?` hoạt động ở đâu
`DeviceError: Into<E>`.

## Nhị phân độc lập

```bash
# Đọc chế độ và tốc độ, không thay đổi gì
cargo run --bin device -- --ip 192.168.12.144 --show

# Chuyển thiết bị sang generator của chính nó, rồi đọc lại
cargo run --bin device -- --ip 192.168.12.144 --mode ping

# Chuyển lại sang lưu lượng bên ngoài (máy tính gửi marker)
cargo run --bin device -- --ip 192.168.12.144 --mode external

# Chỉ in tên chế độ, cho script
cargo run --bin device -- --show --quiet
```

Tùy chọn: `--show`, `--mode MODE`, `--ip`, `--api-port`, `--origin`, `--quiet`,
`-h/--help`. Bắt buộc đúng một trong `--show` / `--mode`.

Mã thoát: `0` thành công, `2` đối số sai hoặc chế độ không hợp lệ, `1` yêu cầu
thất bại, hoặc lệnh ghi không có tác dụng.

`--show` in bốn dòng — thiết bị, chế độ lưu lượng kèm chú thích cho biết nó hàm ý
gì, tốc độ đích, và **bước tiếp theo** (lệnh `collect` khớp với chế độ hiện tại).

## Vì sao đường thu thập không bao giờ ghi

Chế độ tồn tại qua lần khởi động lại và thay đổi ý nghĩa của mọi phép đo, nên nó
là quyết định của người vận hành, không phải tác dụng phụ của việc đo. `collect`,
`streaming` và nhị phân mặc định đọc `GET /sensing` và từ chối lần chạy sai chế
độ; chỉ `cargo run --bin device -- --mode …` mới ghi. Nhờ vậy một capture có thể
tái lập: cùng một lệnh trên cùng một chế độ thiết bị tạo ra cùng loại dữ liệu, và
chế độ sai sẽ dừng lần chạy thay vì lặng lẽ ghi sai nguồn kích thích.

## Yêu cầu

- ESP32 phải truy cập được tại `host:api_port` và đang chạy ESPectre có endpoint
  `/sensing` (phiên bản API báo `traffic_generator_mode`).
- `Origin: https://test.espectre.dev` được gửi kèm mọi yêu cầu; đổi bằng
  `--origin` nếu firmware kiểm tra giá trị khác.
