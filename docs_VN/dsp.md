# Mô-đun DSP

Nguồn: [`src/dsp/mod.rs`](../src/dsp/mod.rs) (+ `preprocess.rs`, `resample.rs`,
`spectral.rs`, `filter.rs`) · Không có nhị phân độc lập (chỉ thư viện)

Xử lý tín hiệu đồng bộ cho các bản ghi CSI đã giải mã. Mô-đun không tự phân
tích tệp `.bin` — nó tiêu thụ các mảng do pipeline
[decoder](decoder.md)/[saving](saving.md) tạo ra thông qua một kiểu lõi duy
nhất, [`CsiSignal`](#csisignal). Mọi hàm DSP đều là các phép biến đổi thuần,
xác định trên mảng `ndarray`: không tokio, không mạng, không I/O tệp trong
bên trong các phép biến đổi (chỉ hai bộ nạp mỏng mới có I/O).

```text
capture_v8.bin ──decoder──▶ CsiPacket[] ──▶ CsiSignal::from_capture
capture_v8.npz ──saving───┘                  │
                                               ▼
                              normalize ──▶ resample ──▶ hampel ──▶ bandpass
                                               │
                                     ┌─────────┴─────────┐
                                     ▼                   ▼
                              fft / dominant_peaks    stft → Spectrogram
```

## `CsiSignal`

```rust
pub struct CsiSignal {
    pub amplitude: Array2<f64>,              // (n_samples, n_subcarriers)
    pub phase: Array2<f64>,                  // thô, chưa xử lý, radian
    pub ticks_us: Array1<u64>,               // device_ticks_us, không giảm
    pub fs: f64,                             // tốc độ lấy mẫu danh nghĩa, Hz
    pub iq: Option<Array2<Complex<f64>>>,    // h_k(t) = I + jQ nếu có
    pub null_bins: Vec<usize>,               // chỉ số subcarrier không mang năng lượng
}
```

`null_bins` liệt kê các subcarrier mà chuẩn quy định là không dùng — với
preamble HT20 là bin DC (32), ba bin bảo vệ ở mỗi biên (0–3 và 61–63), tổng
cộng 8 bin null trên 64. Chúng không mang thông tin kênh: biên độ chỉ là hiện
tượng phần cứng, còn pha là nhiễu vô nghĩa. Mọi đoạn mã phía sau đều bỏ qua các
bin này khi tính thống kê theo trục subcarrier. `from_packets` điền danh sách
bằng `null_bins_for(n_subcarriers)`, trả về bảng HT20 khi
`n_subcarriers == HT20_SUBCARRIERS` (64) và danh sách rỗng trong trường hợp
khác — một chuẩn khác cần bảng khác, nên bảng chỉ nằm ở một nơi thay vì bị
hard-code tại từng chỗ gọi.

Với subcarrier $k$ tại mẫu $t$, đặt $h_k(t) = I_k(t) + j\,Q_k(t)$:

$$|h_k(t)| = \sqrt{I_k(t)^2 + Q_k(t)^2}, \qquad
   \varphi_k(t) = \operatorname{atan2}\bigl(Q_k(t), I_k(t)\bigr)$$

- **Biên độ là tín hiệu chính.** Pha được lưu thô và bọc trong $[-\pi, \pi]$ —
  không bao giờ mở bọc (unwrap) hay làm mịn.
- **`fs` suy ra từ khoảng cách tick trung vị:**

  $$f_s = \frac{10^6}{\operatorname{median}(\Delta\, ticks\_us)}$$

- **Resample nội suy I/Q, không phải biên độ hay pha thô.** Nội suy biên độ làm
  méo bao tín hiệu; nội suy pha thô bị gãy tại điểm bọc $\pm\pi$. Nội suy $I$
  và $Q$ riêng rồi tính lại biên độ/pha sẽ tránh được cả hai.

### Nạp dữ liệu

| Hàm | Đầu vào | Ghi chú |
|---|---|---|
| `CsiSignal::from_capture(path)` | `.bin` thô | vỏ mỏng trên đường decode duy nhất (`decoder` qua `saving`); điền `iq` từ wire |
| `CsiSignal::from_npz(path)` | `.npz` từ [`saving`](saving.md) (`timestamp_ms`, `amplitude_XX`, `phase_XX`) | khôi phục lại ticks và I/Q chính xác từ biên độ+pha |
| `CsiSignal::from_packets(&[CsiPacket])` | các packet đã giải mã | cho caller đã giữ packet sẵn |
| `CsiSignal::new(amplitude, phase, ticks_us)` | mảng | kiểm tra hình dạng, ticks không giảm, tính `fs` |
| `CsiSignal::from_iq(iq, ticks_us)` | mảng phức | suy ra biên độ/pha |

Kiểm tra: không rỗng, hình dạng khớp nhau, `ticks_us` không giảm
(`DspError::NonMonotonicTicks`), và có ít nhất một khoảng tick dương
(`DspError::ZeroVariance` nếu ngược lại).

```rust
use std::path::Path;
use rt_wireless_sensing::dsp::{CsiSignal, DspError, InterpMethod, NormScheme, Window, fft, dominant_peaks};

let signal = CsiSignal::from_capture(Path::new("artifacts/capture_v8.bin"))?;
let signal = signal.normalize(NormScheme::PerSubcarrier);
let signal = signal.resample(20.0, InterpMethod::CubicSpline)?;
let signal = signal.hampel(5, 3.0)?;
let signal = signal.bandpass(0.1, 0.5)?; // dải nhịp thở
for sc in 0..signal.n_subcarriers() {
    let col = signal.column(sc);
    let spectrum = fft(col.as_slice().unwrap(), signal.fs, Window::Hann)?;
    let peaks = dominant_peaks(&spectrum, 0.1, 0.5, 3);
    println!("subcarrier {sc}: {peaks:?}");
}
```

## Tiền xử lý — `src/dsp/preprocess.rs`

### `normalize(NormScheme)`

| Scheme | Công thức |
|---|---|
| `PerSubcarrier` (mặc định) | $z_{t,k} = \dfrac{x_{t,k} - \mu_k}{\sigma_k + \varepsilon}$ |
| `PerSample` (kiểu CSI-Bench) | $z = \dfrac{x - \mu}{\sigma + \varepsilon}$ trên toàn ma trận |
| `MinMax` | $z_{t,k} = \dfrac{x_{t,k} - \min_k}{\max_k - \min_k + \varepsilon}$ |

Hằng số bảo vệ $\varepsilon = 10^{-12}$ giữ cho subcarrier hằng số (phương sai
bằng không) hữu hạn thay vì chia cho không. Chỉ `amplitude` bị biến đổi; pha,
ticks, `fs` và khung nhìn phức vẫn nhất quán.

**Subcarrier null bị loại khỏi mọi thống kê.** `PerSubcarrier` và `MinMax` bỏ
qua các cột null khi tính $\mu_k, \sigma_k, \min_k, \max_k$ và để các cột đó
bằng 0. `PerSample` tính $\mu, \sigma$ chung trên các giá trị không null, rồi
ghi 0 vào các cột null. Nếu tính cả chúng, các bin bảo vệ gần 0 sẽ kéo trung
bình xuống và làm loãng phương sai của mọi subcarrier thật. Với `null_bins`
rỗng, kết quả giống hệt từng bit so với bản không có xử lý null.

### `csi_ratio` (tùy chọn)

Tích liên hợp theo subcarrier — tiền xử lý nhạy pha chuẩn trên nền ESP32. Độ
lệch pha tĩnh bị khử:

$$r_k(t) = h_k(t)\, h_k^*(t-1)
        = |h_k(t)|\,|h_k(t-1)|\; e^{\,j(\varphi_k(t) - \varphi_k(t-1))}$$

Mẫu đầu không có mẫu trước nên đặt $r_k(0) = |h_k(0)|^2$, do đó đầu ra giữ
đúng độ dài đầu vào. Biên độ/pha của kết quả được tính lại từ tích liên hợp.

## Resample — `src/dsp/resample.rs`

`resample(target_fs, method)` nội suy **cả bản ghi một lần** lên lưới đều
$u_j = j / f_s^{target}$ (suy từ thời gian tick tích lũy), rồi tính lại biên
độ/pha từ I/Q đã nội suy. Nội suy cả bản ghi (không chia theo từng giây) tránh
vết gãy tại biên và rò phổ vào STFT.

Tick trùng bị loại (giữ lần đầu); tick lùi là lỗi `DspError::NonMonotonicTicks`.

| `InterpMethod` | Hành vi |
|---|---|
| `Nearest` | hằng từng đoạn (lấy mẫu nguồn gần nhất) |
| `Linear` | nội suy tuyến tính giữa hai mẫu bao quanh |
| `CubicSpline` (mặc định, khuyến nghị cho phân tích phổ) | spline bậc ba tự nhiên, $S''(t_0) = S''(t_{n-1}) = 0$, giải bằng thuật toán Thomas |

Spline bậc ba được viết tay (~40 dòng, kiểm thử bằng đường cong giải tích): không
crate nào được bảo trì mà đánh giá spline bậc ba tự nhiên **không đều** —
Catmull-Rom của crate `splines` đòi hỏi khoảng cách đều, trong khi tick của
thiết bị bị jitter.

### Xử lý khoảng trống — `resample_with`

`resample()` chỉ là lớp bọc mỏng quanh `resample_with(target_fs, method,
options)` — bản triển khai duy nhất. Biến thể có `options` báo cáo thêm **cách**
mỗi điểm lưới được tạo ra:

```rust
pub struct ResampleOptions { pub max_gap_us: Option<u64> }
pub struct ResampleOutput {
    pub signal: CsiSignal,
    pub grid_state: Vec<GridState>,        // một phần tử cho mỗi cột lưới
    pub nominal_interval_us: f64,
}
pub enum GridState { Observed = 0, Bridged = 1, Masked = 2 }
```

Một khoảng mất dữ liệu dài hơn một chu kỳ lấy mẫu không phải là đặc trưng tín
hiệu, nên nguồn được **chia thành các đoạn tại mỗi khoảng trống vượt ngưỡng** và
mỗi đoạn được nội suy độc lập; hai đoạn kề nhau dùng chung mẫu biên nên không
mất dữ liệu. Cột lưới chỉ được ghi khi nó nằm trong một đoạn — khoảng trống
không bao giờ bị ngoại suy, đó chính là điều ngăn spline tự vẽ một cầu nối mượt
qua chỗ mất 250 ms.

Mỗi cột được ghi sau đó được phân loại theo `nominal_interval_us` (trung vị chu
kỳ nguồn):

| `GridState` | Điều kiện | Ý nghĩa |
|---|---|---|
| `Observed` | khoảng nguồn $\le 1.5 \times$ trung vị | dữ liệu thật, chỉ nội suy do jitter |
| `Bridged` | giữa ngưỡng 1.5× và `max_gap_us` | nội suy qua chỗ mất mát thật — hợp lý nhưng là dữ liệu dựng lại |
| `Masked` | khoảng $> $ `max_gap_us` | bị che: quá dài để tin, được đánh dấu cho phân tích |

Với `max_gap_us == None` không cột nào bị che, nhưng `Observed`/`Bridged` vẫn
được phân biệt. Phía tiêu thụ hiển thị `Masked` dưới dạng NaN (hoặc ô CSV rỗng)
thay vì một con số, để một lỗ hổng nhìn thấy được không bao giờ trông giống một
phép đo.

## Phân tích phổ — `src/dsp/spectral.rs`

FFT đầu vào thực dùng crate [`realfft`](https://crates.io/crates/realfft) (phổ
một phía có ngay, nhanh gấp ~2 lần FFT phức, dùng chung `rustfft` 6.4). Với
tín hiệu thực độ dài $N$, tần số lấy mẫu $f_s$:

$$X_k = \sum_{n=0}^{N-1} w_n\, x_n\, e^{-j 2\pi k n / N}, \qquad
   f_k = \frac{k f_s}{N}, \quad k = 0 \dots \lfloor N/2 \rfloor$$

### `fft(signal, fs, window) -> Spectrum`

Loại thành phần DC (trung bình), áp cửa sổ $w_n$, trả về phổ một phía cho các
bin $k = 1 \dots \lfloor N/2 \rfloor$ với hệ số biên độ

$$|X_k| \mapsto \frac{2\,|X_k|}{\sum_n w_n}$$

(bin Nyquist của tín hiệu độ dài chẵn không nhân đôi). Với cửa sổ Hann
$\sum w_n \approx N/2$, nên sóng sin thuần biên độ $A$ đạt đỉnh $\approx A$.

| `Window` | Công thức (tuần hoàn, mẫu số $N$) |
|---|---|
| `None` | $w_n = 1$ |
| `Hann` (mặc định) | $w_n = 0.5 - 0.5\cos(2\pi n / N)$ |
| `Hamming` | $w_n = 0.54 - 0.46\cos(2\pi n / N)$ |
| `Blackman` | $w_n = 0.42 - 0.5\cos(2\pi n/N) + 0.08\cos(4\pi n/N)$ |

- `Spectrum { freqs, magnitude, complex }` — `freqs[k] = (k+1) f_s / N`.
- `freq_bins(n, fs)` — các bin một phía $f_k = k f_s / N$, $k = 0..\lfloor N/2\rfloor$.
- `dominant_peaks(&spectrum, min_hz, max_hz, top_k)` — `top_k` cặp `(freq, magnitude)`
  trong dải do người gọi chỉ định (thay dải 0.2–10 Hz cố định cũ): 0.1–0.5 Hz
  cho nhịp thở, 0.2–10 Hz cho chuyển động.
- `signal.fft_subcarriers(window)` — phổ từng subcarrier.

### `stft(signal, fs, &StftConfig) -> Spectrogram`

Trượt cửa sổ với bước nhảy và xếp các phổ một phía của từng khung:

$$S(m, k) = \sum_{n=0}^{W-1} w_n\, x_{mH+n}\, e^{-j 2\pi k n / W}$$

- `StftConfig { window_len, hop, window }` — mặc định: Hann, overlap 50%
  (`hop = window_len / 2`); `StftConfig::with_window(len, window)` cho ca phổ
  biến.
- `Spectrogram { times, freqs, magnitudes }` — `magnitudes` có dạng
  `(n_freqs, n_frames)` (tần số × thời gian); `times[j] = (start_j + W/2)/f_s`
  là tâm khung theo giây. `to_db()` đổi biên độ bằng
  $20\log_{10}(\max(m, 10^{-12}))$.
- Một FFT đã plan được tái sử dụng cho mọi khung và mọi subcarrier.
- Hướng dẫn chọn độ dài cửa sổ: cửa sổ dài cho độ phân giải tần tốt, cửa sổ
  ngắn cho độ phân giải thời gian tốt.

| Tín hiệu quan tâm | Độ dài cửa sổ | Tại tốc độ 20 Hz |
|---|---|---|
| Nhịp thở (0.1–0.5 Hz) | 10–20 s | 200–400 mẫu |
| Chuyển động / cử chỉ (0.2–10 Hz) | 0.5–1 s | 10–20 mẫu |

- `signal.stft_subcarriers(&config)` — spectrogram từng subcarrier.

## Lọc — `src/dsp/filter.rs`

### Loại outlier (miền thời gian, từng subcarrier)

Đều dùng cửa sổ giữa độ dài `k` mẫu (làm tròn lên lẻ), thu nhỏ ở biên.

| Hàm | Công thức |
|---|---|
| `moving_average(x, k)` | $y_i = \frac{1}{|W_i|}\sum_{j \in W_i} x_j$ (O(n) nhờ prefix sum) |
| `exp_moving_average(x, k)` | $y_i = \alpha x_i + (1-\alpha) y_{i-1}$, $\alpha = 2/(k+1)$ |
| `median_filter(x, k)` | $y_i = \operatorname{median}(W_i)$ |
| `hampel_filter(x, window, n_sigma)` | thay $x_i$ bằng $m_i$ khi $\|x_i - m_i\| > n_\sigma\, \sigma_i$ |

Hampel là bộ loại outlier CSI được dùng nhiều nhất trong các khảo sát, với độ
lệch chuẩn bền (dựa trên MAD):

$$m_i = \operatorname{median}(W_i), \qquad
   \sigma_i = 1.4826 \cdot \operatorname{median}_{j \in W_i} |x_j - m_i|$$

Trung vị được viết tay bằng quickselect (`select_nth_unstable`): API quantile
của `ndarray-stats` 0.7 là in-place trên `ArrayRef` và không khớp với cửa sổ
trượt chỉ đọc trên `&[f64]`, trong khi kích thước cửa sổ nhỏ.

### Biquad miền tần số

`lowpass`, `highpass`, `bandpass` áp các biquad bậc hai RBJ qua crate
[`biquad`](https://crates.io/crates/biquad) (`DirectForm2Transposed`, Butterworth
$Q = 1/\sqrt{2}$):

$$y[n] = b_0 x[n] + b_1 x[n-1] + b_2 x[n-2] - a_1 y[n-1] - a_2 y[n-2]$$

`bandpass(x, fs, lo, hi)` là high-pass tại `lo` nối tiếp low-pass tại `hi`. Lưu ý
transient khởi động (vài mẫu đầu chưa ở trạng thái dừng). Ứng dụng điển hình:
BPF 0.1–0.5 Hz để cô lập nhịp thở; HPF trên ~0.2 Hz để loại phản xạ từ vật
tĩnh trước khi phân tích chuyển động/cử chỉ.

Mỗi hàm trên lát (slice) đều có một method `CsiSignal` áp cho mọi cột biên độ:
`signal.hampel(5, 3.0)`, `signal.lowpass(5.0)`, `signal.bandpass(0.1, 0.5)`, …

## Cách chọn

| Mục tiêu | Chuỗi gợi ý |
|---|---|
| Nhịp thở / dấu hiệu sinh tồn (0.1–0.5 Hz) | `bandpass(0.1, 0.5)` → `fft` + `dominant_peaks` |
| Chuyển động / cử chỉ (0.2–10 Hz) | `hampel` → `highpass(0.2)` → `stft` |
| Dọn dữ liệu nói chung | `hampel` → `normalize(PerSubcarrier)` |
| So sánh bản ghi có tốc độ packet khác nhau | `resample(fs, CubicSpline)` trước |

## Lỗi

`DspError` là enum cụ thể (cùng phong cách với `CollectionError`/`SaveError`),
không có `Box<dyn Error>` trong chữ ký công khai:

| Biến thể | Nguyên nhân |
|---|---|
| `Io` / `Decode` / `Npz` | lỗi tệp, bản ghi hoặc kho lưu trữ (đóng gói nguồn) |
| `MissingArray(name)` | thiếu mảng bắt buộc trong `.npz` |
| `EmptySignal` | không có mẫu nào |
| `InvalidShape(msg)` | hình dạng mảng không khớp |
| `NonMonotonicTicks { index, previous, found }` | tick bị lùi |
| `ZeroVariance(msg)` | ví dụ mọi khoảng tick đều bằng 0 nên không có tốc độ mẫu |
| `InvalidConfig(msg)` | `fs`, độ dài cửa sổ, tần số cắt, dải… không hợp lệ |

## Ngoài phạm vi (tương lai)

- DWT/wavelet và PCA/nén — hoãn lại.
- Tích hợp với [`visualization`](visualization.md) — dự kiến khi API lõi ổn
  định; `Spectrum`/`Spectrogram` là struct dữ liệu thuần nên lớp render có thể
  dùng mà không phụ thuộc vào bên trong dsp.
- Tích hợp streaming — không phải trách nhiệm của mô-đun này.

## Kiểm thử

Toàn bộ kiểm thử dùng dữ liệu tổng hợp (không bản ghi, không fixture): kiểm thử
đơn vị trong từng mô-đun con (hằng số bảo vệ epsilon của chuẩn hóa, độ chính xác
spline so với tuyến tính trên đường cong trơn, đỉnh FFT trong một bin và khôi
phục biên độ Hann, vệt STFT của sóng sin/méo tần, khôi phục xung của
Hampel/median, suy hao LPF/BPF tính bằng dB) cộng kiểm thử tích hợp trong
[`tests/dsp.rs`](../tests/dsp.rs) chạy cả chuỗi
`resample → normalize → hampel → bandpass → stft` và round-trip `.npz`
theo đúng layout của `saving`. Các ví dụ trong tài liệu được kiểm tra biên dịch
(`cargo test --doc`).
