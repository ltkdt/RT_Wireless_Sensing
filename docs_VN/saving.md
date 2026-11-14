# Mô-đun lưu trữ

Nguồn: [`src/saving.rs`](../src/saving.rs) · Nhị phân độc lập:
[`src/bin/save.rs`](../src/bin/save.rs)

Mô-đun này chuyển đổi bản ghi nhị phân đã thu thập thành mảng số để phân tích
Python hoặc Rust. Nó giải mã bản ghi với [decoder](decoder.md) và
ghi một kho lưu trữ `.npz` bên cạnh đầu vào, giữ nguyên gốc file stem.

```text
capture_v8.bin  ──decode──►  capture_v8.npz
                             ├─ timestamp_ms   (N,)
                             ├─ amplitude_00…63 (N,)
                             └─ phase_00…63     (N,)
```

## Bố cục đầu ra

`N` là số bản ghi đã giải mã. Bản ghi HT20 ESPectre tiêu chuẩn có 64
subcarrier phức, sản xuất **1 + 64 + 64 = 129 mảng**:

| Tên mảng | Hình dạng | dtype | Nội dung |
|---|---|---|---|
| `timestamp_ms` | `(N,)` | `float64` | Thời gian mẫu tính bằng miligiây, bản ghi đầu tiên = `0.0` |
| `amplitude_00` … `amplitude_63` | `(N,)` | `float32` | `hypot(I, Q)` cho subcarrier đó |
| `phase_00` … `phase_63` | `(N,)` | `float32` | `atan2(Q, I)` cho subcarrier đó, radian |

- Chỉ số subcarrier dựa trên zero và khớp với thứ tự payload của bộ giải mã
  (HT20 có trung tâm: bin 0 là guard/null bin đầu tiên).
- Biên độ và pha được suy ra từ quy ước `(I, Q)` của bộ giải mã:
  `payload[0] = Q`, `payload[1] = I`.
- Pha là giá trị bọc thô trong `[−π, π]`; nó **không** được mở bọc hoặc
  làm mịn.
- Subcarrier null được bảo toàn dưới dạng biên độ zero / pha zero; chúng không
  bị xóa.
- Nếu bản ghi sử dụng `num_sc` nhất quán khác, kho lưu trữ có
  `1 + 2 * num_sc` mảng và tên chỉ số vẫn được điệp hai chữ số bằng zero.
  Bản ghi có `num_sc` thay đổi giữa dòng bị từ chối.

## Timestamp

Định dạng wire thô không có trường đồng hồ tường. Mỗi mẫu mang theo
64-bit `device_ticks_us` đơn điệu, vì vậy timestamp được tính tương đối so với bản ghi đầu tiên:

```text
timestamp_ms[i] = (device_ticks_us[i] - device_ticks_us[0]) / 1000
```

Hệ quả:

- Giá trị đầu tiên chính xác là `0.0`.
- Đơn vị là miligiây (`f64`); ở 20 PPS khoảng cách là khoảng 50 ms.
- Thời gian là thời gian mẫu từ thiết bị, không phải thời gian đến HTTP, vì vậy
  jitter mạng và thời gian giải mã không ảnh hưởng đến chúng.
- `device_ticks_us` phải khác không và không giảm. Bản ghi vi phạm
  điều này thất bại với lỗi thay vì ghi timestamp sai lệch.
- Giá trị tick tuyệt đối gốc không được lưu trữ; chỉ có thời gian tương đối.

`wifi_rx_ts_us` cố tình không được sử dụng: nó chỉ hiện diện khi cờ hợp lệ
của nó được đặt, là 32-bit (nó quấn sau khoảng 71 phút), và trên một số chip sử dụng
một đồng hồ khác với `device_ticks_us`.

## API quan trọng

### `save_capture`

```rust
pub fn save_capture(bin_path: &Path) -> Result<SaveSummary, SaveError>
```

Giải mã `bin_path` và ghi `npz_path_for(bin_path)`.

```rust
use std::path::Path;
use rt_wireless_sensing::saving::save_capture;

let summary = save_capture(Path::new("capture_v8.bin"))?;
println!("{} records -> {}", summary.records, summary.output.display());
```

### `save_capture_to`

```rust
pub fn save_capture_to(
    bin_path: &Path,
    npz_path: &Path,
) -> Result<SaveSummary, SaveError>
```

Giống như `save_capture`, nhưng người gọi chọn đường dẫn kho lưu trữ. Hữu ích cho
công việc hàng loạt hoặc thử nghiệm.

Đối số:

- `bin_path` — bản ghi thô được sản xuất bởi [`collection`](collection.md), ví dụ `capture_v8.bin`.
- `npz_path` — kho lưu trữ đích. Thư mục cha phải tồn tại.

Đầu ra: [`SaveSummary`](#savesummary), hoặc [`SaveError`](#saveerror).

### `npz_path_for`

```rust
pub fn npz_path_for(bin_path: &Path) -> PathBuf
```

Thay thế phần mở rộng bằng `.npz`: `capture_v8.bin` → `capture_v8.npz`,
`data/run.bin` → `data/run.npz`. Đường dẫn không có phần mở rộng được thêm `.npz`.

### `SaveSummary`

| Trường | Kiểu | Ý nghĩa |
|---|---|---|
| `input` | `PathBuf` | Nguồn `.bin` |
| `output` | `PathBuf` | `.npz` đã ghi |
| `records` | `usize` | Bản ghi đã giải mã (`N`) |
| `subcarriers` | `usize` | Subcarrier phức mỗi bản ghi |
| `arrays` | `usize` | `1 + 2 * subcarriers` |
| `duration_ms` | `f64` | Timestamp cuối cùng tính bằng ms |
| `pps` | `f64` | Tốc độ hiệu quả từ tick thiết bị |
| `bytes_written` | `u64` | Kích thước tệp `.npz` |

### `SaveError`

| Biên thể | Nguyên nhân |
|---|---|
| `Io(std::io::Error)` | Đọc đầu vào hoặc ghi kho lưu trữ thất bại |
| `Decode(DecodeError)` | Bản ghi bị sai hình thức hoặc bị cắt xén |
| `NoRecords` | Không có bản ghi CSI hoàn chỉnh trong tệp |
| `NoSubcarriers { record }` | Bản ghi báo cáo zero subcarrier |
| `MissingDeviceTicks { record }` | `device_ticks_us == 0`, vì vậy không có thời gian tương đối |
| `NonMonotonicDeviceTicks { record, previous, found }` | Đồng hồ thiết bị chạy ngược |
| `SubcarrierCountChanged { record, expected, found }` | Bố cục thay đổi giữa bản ghi |
| `WriteNpz(WriteNpzError)` | Lỗi ghi ZIP/NPY |

## Mô-đun độc lập

```bash
# capture_v8.bin -> capture_v8.npz
cargo run --bin save -- capture_v8.bin

# Chọn đường dẫn kho lưu trữ
cargo run --bin save -- capture_v8.bin --output arrays.npz

# Trợ giúp
cargo run --bin save -- --help
```

Đầu vào mặc định là `capture_v8.bin`. Đầu ra mong đợi cho bản ghi kho:

```text
input        : capture_v8.bin
output       : capture_v8.npz (389274 bytes)
records      : 691
subcarriers  : 64
arrays       : 129
duration     : 34499.075 ms
rate         : 20.00 records/s
```

## Đọc kết quả

Python:

```python
import numpy as np

with np.load("capture_v8.npz") as data:
    print(len(data.files))          # 129
    t = data["timestamp_ms"]        # (691,) float64, bắt đầu từ 0.0
    amp = data["amplitude_04"]      # (691,) float32, subcarrier đầu tiên hoạt động
    phase = data["phase_04"]
    matrix = np.stack([data[f"amplitude_{k:02d}"] for k in range(64)], axis=1)
```

Rust:

```rust
use ndarray::Array1;
use ndarray_npy::NpzReader;
use std::fs::File;

let mut npz = NpzReader::new(File::open("capture_v8.npz")?)?;
let timestamps: Array1<f64> = npz.by_name("timestamp_ms")?;
let amplitude: Array1<f32> = npz.by_name("amplitude_04")?;
```

## Ghi chú

- Nhà văn sử dụng `ndarray-npy`, đã là phụ thuộc của gói này.
- Kho lưu trữ bị sai hình thức không được sửa im lặng: `save_capture_to` trả về
  lỗi bộ giải mã với offset byte của nó.
- Mô-đun này không thực hiện DSP, lọc, hoặc suy luận. Nó chỉ xuất
  timestamp, biên độ, và giá trị pha đã giải mã.
