# Lyra Turntable

Đầu đĩa than kiểu cổ điển cho **foobar2000** trên Windows. Lyra chỉ là phần giao diện: mọi việc giải mã và xuất âm thanh vẫn do foobar2000 làm. Lyra điều khiển foobar2000 qua **Beefweb** (REST API ở `localhost:8880`) và qua dòng lệnh của foobar2000.

- Tauri v2 (Rust + WebView2), React 18, TypeScript, Vite
- Cửa sổ trong suốt, không viền, kéo được bằng chuột trái
- Phiên bản hiện tại: **0.3.0**

> Kế hoạch thay foobar2000 bằng engine riêng: xem [docs/PLAN-replace-foobar2000.md](docs/PLAN-replace-foobar2000.md).

---

## 1. Yêu cầu

| Thành phần | Ghi chú |
|---|---|
| Windows 10/11 x64 | Có sẵn WebView2 |
| foobar2000 v2 x64 | Cài chuẩn tại `C:\Program Files\foobar2000\foobar2000.exe` (đường dẫn đang hard-code trong `src-tauri/src/lib.rs`) |
| `foo_beefweb` (0.10+) | **Bắt buộc**: Lyra nói chuyện với foobar2000 qua component này |
| `foo_out_asio` | Tuỳ chọn: xuất qua driver ASIO của DAC |
| `foo_input_sacd` | Tuỳ chọn: đọc DSD (`.dsf`, `.dff`, SACD `.iso`) |
| `foo_dsd_processor` | Tuỳ chọn: xử lý/chuyển đổi DSD |

### Cấu hình Beefweb
foobar2000 → *File › Preferences › Tools › Beefweb Remote Control*:
- **Port**: `8880`
- **Allow remote connections**: tắt (Lyra chỉ cần `localhost`)
- **Require authentication**: tắt

---

## 2. Cài đặt

**Cách 1: dùng bộ cài**
Chạy `Lyra Turntable_0.3.0_x64-setup.exe`. App được cài cho riêng user hiện tại tại `%LOCALAPPDATA%\Lyra Turntable\lyra.exe`, không cần quyền admin.

**Cách 2: build từ mã nguồn**
```powershell
# cần: Node 20+, Rust (msvc), VS Build Tools 2022 (C++ workload + Windows SDK)
npm install
npm run tauri dev     # chạy thử
npx tauri build       # ra bộ cài ở src-tauri\target\release\bundle\nsis\
```
Đổi icon: sửa `app-icon.svg` rồi chạy `npx tauri icon app-icon.svg`.

---

## 3. Phát file high-end mà vẫn giữ nguyên chất lượng (bit-perfect)

Mục tiêu là đưa **đúng từng bit trong file** tới DAC, không để Windows hay phần mềm nào resample, trộn tiếng hay giảm âm lượng bằng số. Lyra không động vào dữ liệu âm thanh, nên mọi thiết lập dưới đây nằm trong **foobar2000** và **Windows**.

> Tên các mục menu có thể lệch chút tuỳ phiên bản foobar2000 và component.

### 3.1 Chọn đường ra độc quyền (exclusive)
foobar2000 → *Preferences › Playback › Output*:
- **Device**: chọn một trong hai
  - `WASAPI (exclusive) : <tên DAC>`: chuẩn của Windows, DAC nào cũng dùng được.
  - `ASIO : <driver của DAC>`: cần `foo_out_asio` và driver ASIO của hãng DAC. Thường ổn định hơn, và là cách duy nhất để phát **DSD native**.
- **Output format**: chọn độ sâu bit cao nhất DAC hỗ trợ (24-bit hoặc 32-bit).
- **Buffer length**: khoảng 1000 ms. Tăng lên nếu nghe thấy tiếng lụp bụp.

**Tránh dùng** `Primary Sound Driver` hay `DirectSound`. Hai đường này đi qua bộ trộn của Windows, nên âm thanh bị resample về sample rate mặc định của hệ thống.

### 3.2 Không để gì xử lý tín hiệu
- *Preferences › Playback › DSP Manager*: danh sách **Active DSPs để trống** (không resampler, không equalizer, không crossfeed).
- *Preferences › Playback › ReplayGain*: **Source mode = none**.
- **Âm lượng foobar2000 để ở 0 dB (mức tối đa)**. Núm volume trên Lyra chính là volume số của foobar2000: vặn nhỏ đi là cắt bit, không còn bit-perfect. Hãy chỉnh âm lượng bằng núm trên DAC hoặc ampli.

### 3.3 Windows
*Control Panel › Sound › <DAC> › Properties*:
- *Advanced*: bật **Allow applications to take exclusive control of this device**.
- *Enhancements* hoặc *Audio enhancements*: **tắt hết**.
- *Spatial sound*: **Off**.

### 3.4 PCM hi-res (FLAC 24/96, 24/192, WAV, ALAC, AIFF, WavPack, APE)
Không cần làm gì thêm. Khi dùng WASAPI exclusive hoặc ASIO, DAC tự chuyển sang đúng sample rate của từng bài. Đèn hoặc màn hình trên DAC sẽ đổi theo, ví dụ 44.1 → 96 → 192 kHz.

### 3.5 DSD (`.dsf`, `.dff`, SACD `.iso`)
Cần `foo_input_sacd`. Trong *Preferences › Tools › SACD* (hoặc *DSD*), **Output Mode** có ba lựa chọn:

| Chế độ | Khi nào dùng | Đường ra |
|---|---|---|
| **PCM** | DAC không hỗ trợ DSD | Bất kỳ. DSD được chuyển sang PCM, không còn là DSD gốc |
| **DSD (DoP)** | DAC hỗ trợ DoP (đa số DAC USB đời mới) | WASAPI exclusive 24-bit, hoặc ASIO |
| **DSD native** | DAC có driver ASIO hỗ trợ DSD | ASIO |

Kiểm tra: khi phát file DSD, DAC phải báo **DSD64** hoặc **DSD128**…, chứ không báo 176.4 kHz PCM. Nếu DAC báo PCM ở chế độ DoP, nguyên nhân thường là âm lượng không ở 0 dB hoặc có DSP nào đó đang can thiệp vào dữ liệu DoP.

SACD `.iso` không có trong menu chuột phải *Open with*. Hãy dùng **Tệp › Thêm tệp** hoặc kéo thả file vào Lyra.

### 3.6 Checklist nhanh
- [ ] Output = WASAPI (exclusive) hoặc ASIO
- [ ] DSP Manager trống, ReplayGain = none
- [ ] Volume foobar2000 / Lyra = 0 dB, chỉnh âm lượng trên DAC
- [ ] Windows: cho phép exclusive, tắt enhancements
- [ ] DSD: chọn DoP hoặc native, DAC hiện chữ DSD

---

## 4. Dùng Lyra

| Việc | Cách làm |
|---|---|
| Mở một file nhạc | Chuột phải file › **Open with › Lyra Turntable**, hoặc **Tệp › Mở…** (Ctrl+O). File được phát trong *Default Playlist* |
| Thêm vào playlist đang xem | **Tệp › Thêm tệp / Thêm thư mục / Thêm địa chỉ (URL)**, hoặc kéo thả vào cửa sổ |
| Playlist | **Tệp › Playlist mới** (Ctrl+N), **Mở/Lưu playlist** (Ctrl+S). Chọn playlist ở ô chọn phía trên danh sách. Nút × xoá từng bài, **Xóa hết** xoá cả danh sách |
| Tua (needle drop) | Bấm vào một rãnh trên đĩa |
| Điều khiển | Phím đàn PREV / PLAY / NEXT, núm volume (nhớ đọc mục 3.2) |
| Ẩn/hiện danh sách | Bấm mũi tên ‹ / › ở mép giữa đĩa và danh sách |
| Di chuyển cửa sổ | Nhấn giữ **chuột trái** ở bất kỳ chỗ trống nào rồi kéo |
| Ghim trên cùng / thu nhỏ / đóng | Ba nút góc trên bên phải |
| Khay hệ thống | Play/Pause, Next, Prev, Hiện Lyra, Hiện/ẩn foobar2000, Thoát |
| Cài đặt foobar2000 | **Tệp › Tuỳ chọn** (Ctrl+P) mở Preferences của foobar2000 |

### Cách Lyra xử lý foobar2000
- Nếu foobar2000 **chưa chạy**: Lyra tự khởi động nó và **ẩn hoàn toàn** cửa sổ của nó. Khi bạn đóng Lyra, foobar2000 cũng được đóng theo.
- Nếu foobar2000 **đang chạy sẵn** (do bạn tự mở): Lyra dùng chung và **không** đóng nó khi Lyra thoát.

### Default Playlist
- Mỗi lần khởi động, *Default Playlist* được làm trống, **trừ khi** bạn đã tự thêm bài vào đó hoặc nó đang phát.
- File mở bằng *Open with* hay *Tệp › Mở* luôn vào *Default Playlist* và phát ngay.

---

## 5. Xử lý sự cố

| Triệu chứng | Kiểm tra |
|---|---|
| Lyra báo không kết nối được | foobar2000 có đang chạy không? Beefweb đã bật ở port 8880 chưa? Mở thử `http://localhost:8880/api/player` |
| Không thấy Lyra trong *Open with* | Chạy Lyra một lần (lần chạy đầu sẽ đăng ký). Lyra không bao giờ tự đặt mình làm app mặc định |
| Icon cũ vẫn hiện | Do cache icon của Windows. Đăng xuất rồi đăng nhập lại |
| Âm thanh lụp bụp khi dùng exclusive | Tăng Buffer length, dùng ASIO nếu DAC có driver |
| DAC không đổi sample rate | Đang dùng DirectSound/Primary hoặc có resampler trong DSP. Xem lại mục 3 |

---

## 6. Cấu trúc mã

```
src/
  App.tsx         khung cửa sổ, menu Tệp, playlist, drawer, kéo cửa sổ, Open-with
  Turntable.tsx   đầu đĩa SVG (đĩa, tay đọc, phím, núm volume)
  player.ts       hook useFoobar: gọi Beefweb, poll trạng thái mỗi 1 giây
  styles.css
src-tauri/
  src/lib.rs      vòng đời foobar2000, ẩn cửa sổ (Win32), khay hệ thống,
                  single-instance, đăng ký Open-with (HKCU)
  tauri.conf.json
  capabilities/default.json
docs/
  PLAN-replace-foobar2000.md
```
