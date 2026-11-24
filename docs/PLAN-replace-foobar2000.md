# Kế hoạch: thay foobar2000 bằng engine âm thanh riêng (Rust)

Trạng thái: **đề xuất, chưa bắt đầu**. Đang hoãn theo quyết định ngày 2026-11-24.

## 0. Trước hết: thay để làm gì?

Cần nói rõ một điểm dễ hiểu lầm: **một engine tự viết sẽ không làm nhạc hay hơn foobar2000**. Khi đã cấu hình bit-perfect (WASAPI exclusive hoặc ASIO, 0 dB, không DSP), foobar2000 đưa nguyên từng bit tới DAC. Engine mới tối đa chỉ làm được *bằng*, không *hơn*.

Lý do thật sự để thay:

| Được | Mất / rủi ro |
|---|---|
| Còn một tiến trình duy nhất, không phải ẩn cửa sổ, không phụ thuộc đường dẫn cài foobar2000 hay Beefweb | Mất 20 năm độ chín của foobar2000: xử lý file lỗi, tag lạ, cue sheet, gapless, hàng trăm component |
| Kiểm soát trọn vẹn: hiện format thật đang ra DAC, khoá volume, chế độ bit-perfect có kiểm chứng | Phải tự lo DSD native, driver ASIO, các ca biên của WASAPI exclusive |
| Sự kiện push thay cho poll mỗi giây, UI phản hồi tức thì | Thời gian: ước tính 6–10 tuần làm buổi tối cho bản tương đương phần đang dùng |
| Dễ phân phối: một bộ cài, không cần hướng dẫn cài component | APE, WavPack, Opus không có decoder Rust thuần. Phải dùng FFmpeg (LGPL) hoặc bỏ |

**Quyết định đề xuất:** làm theo kiểu **song song**. Engine mới được đưa vào như một *backend thứ hai* sau cùng một giao diện. foobar2000 vẫn là backend dự phòng cho tới khi engine mới qua đủ các bài kiểm tra ở mục 5.

## 1. Kiến trúc đích

```
React UI (giữ nguyên Turntable.tsx, App.tsx)
   │  PlayerBackend interface (TS)
   ├── FoobarBackend   ← player.ts hiện tại (Beefweb HTTP + CLI)
   └── NativeBackend   ← Tauri invoke() + event "player://state"
                            │
Rust (src-tauri)            ▼
 ┌─────────────── engine crate ────────────────┐
 │ Control thread: lệnh play/pause/seek/queue │
 │   │ crossbeam channel                      │
 │ Decode thread: Source → PCM/DSD frames     │
 │   │ rtrb ring buffer (lock-free, ~2 s)     │
 │ Output thread (real-time priority, MMCSS): │
 │   WASAPI exclusive │ ASIO │ WASAPI shared  │
 └────────────────────────────────────────────┘
 Library: SQLite (rusqlite) + lofty (tag) + notify (theo dõi thư mục)
```

Nguyên tắc: **đường từ decoder tới DAC không đổi một mẫu nào**. Không resample, không volume số, không dither, trừ khi người dùng bật rõ ràng và UI báo "không còn bit-perfect".

## 2. Chọn thư viện

| Việc | Crate | Ghi chú / rủi ro |
|---|---|---|
| Giải mã FLAC, WAV, AIFF, ALAC, MP3, AAC, Vorbis | `symphonia` | Rust thuần, đủ cho phần lớn thư viện nhạc |
| APE, WavPack, Opus | `ffmpeg-next` (link động FFmpeg) | Giấy phép LGPL, tăng kích thước bộ cài. Có thể để giai đoạn sau |
| DSF / DFF | Tự viết parser (đơn giản, có spec công khai) | DFF có thể nén DST (hiếm, khó). Giai đoạn đầu chỉ hỗ trợ DFF không nén |
| SACD `.iso` | **Không làm** giai đoạn đầu | Cấu trúc phức tạp, thường nén DST. Gợi ý: tách ISO ra DSF bằng công cụ ngoài |
| WASAPI exclusive | `wasapi` (crate của HEnquist, dùng trong CamillaDSP) | `cpal` **không** hỗ trợ exclusive mode, không dùng được cho mục tiêu bit-perfect |
| ASIO | `asio-sys` / `cpal` feature `asio` | Cần ASIO SDK của Steinberg khi build (không được phân phối lại SDK) |
| Ring buffer | `rtrb` | Lock-free, an toàn cho thread real-time |
| Tag, ảnh bìa | `lofty` | |
| Resample (chỉ khi DAC không nhận rate gốc) | `rubato` | Tắt mặc định |
| Thư viện nhạc | `rusqlite`, `notify` | |

## 3. DSD: phần khó nhất

- **DoP (DSD over PCM)**: gói 16 bit DSD vào mỗi mẫu 24-bit, 8 bit cao là marker `0x05`/`0xFA` luân phiên. DSD64 (2.8224 MHz) thành PCM 176.4 kHz, DSD128 thành 352.8 kHz. Chạy được qua WASAPI exclusive 24-bit. **Không được** đụng vào volume, nếu không marker vỡ và DAC phát ra tiếng ồn.
- **Native DSD**: chỉ qua ASIO, với driver hãng có hỗ trợ (`ASIOSetIoFormat` với `kASIODSDFormat`). Mỗi hãng một kiểu, phải test theo từng DAC.
- **DSD → PCM** (cho DAC không hỗ trợ DSD): lọc FIR rồi decimate. Đây là DSP, không bit-perfect, UI phải ghi rõ.

## 4. Lộ trình

| Giai đoạn | Nội dung | Tiêu chí xong |
|---|---|---|
| **P0 — Tách giao diện** (2–3 tối) | Tạo `PlayerBackend` trong TS. `player.ts` thành `FoobarBackend`. Không đổi hành vi | App chạy y như 0.3.0 |
| **P1 — Phát PCM cơ bản** (1–2 tuần) | `symphonia` → `rtrb` → WASAPI **exclusive**. Play/pause/seek/next. Đổi sample rate theo từng bài. Hiện format đang ra DAC | FLAC 16/44, 24/96, 24/192 phát đúng, DAC đổi rate |
| **P2 — Playlist & queue** (1 tuần) | Playlist lưu SQLite hoặc JSON, Default Playlist như logic hiện tại, Open-with/drag-drop gọi thẳng engine | Mọi chức năng menu Tệp chạy không cần foobar2000 |
| **P3 — Gapless & cue** (1 tuần) | Decode trước bài kế tiếp. Cùng format thì nối liền, khác format thì mở lại stream. Cue sheet tách track ảo | Album live/classical không bị khựng giữa bài |
| **P4 — DSD** (1–2 tuần) | Parser DSF/DFF, DoP qua WASAPI. Native DSD qua ASIO (tuỳ DAC) | DAC báo DSD64/128, không có tiếng xè |
| **P5 — ASIO** (1 tuần) | Backend ASIO cho PCM và DSD native | Chạy với driver của DAC đang dùng |
| **P6 — Thư viện** (tuỳ chọn) | Quét thư mục, tag, ảnh bìa, tìm kiếm | |
| **P7 — Bỏ foobar2000** | NativeBackend thành mặc định. FoobarBackend giữ ở Settings vài phiên bản rồi xoá | Mục 5 qua hết trong 2 tuần nghe thật |

## 5. Kiểm chứng bit-perfect (bắt buộc trước P7)

1. **So sánh dữ liệu**: chế độ debug ghi đúng buffer gửi cho WASAPI ra file, so hash với PCM do `ffmpeg -f s24le` giải mã từ cùng file. Hai bên phải trùng từng byte.
2. **File test của DAC**: các file "bit-perfect test" (HDCD/MQA/DTS-in-WAV) chỉ làm đèn chỉ báo trên DAC sáng khi dữ liệu không bị sửa.
3. **DoP**: DAC phải hiện DSD, không phải PCM 176.4/352.8 kHz.
4. **Độ ổn định**: phát liên tục 8 giờ trong lúc dùng máy bình thường, không xảy ra underrun (đếm số lần underrun trong log).
5. **Ca biên**: rút DAC ra lúc đang phát, app khác chiếm thiết bị, máy ngủ rồi thức dậy, file hỏng hoặc thiếu header.

## 6. Rủi ro và cách giảm

| Rủi ro | Giảm thiểu |
|---|---|
| Underrun khi máy bận | Thread output dùng MMCSS "Pro Audio", buffer ~2 s ở phía decode, đo latency theo từng DAC |
| Exclusive mode làm app khác mất tiếng | Đây là bản chất của exclusive. Cho phép chuyển sang shared mode và hiện cảnh báo trên UI |
| Driver ASIO mỗi hãng một kiểu | Giữ WASAPI exclusive làm mặc định, ASIO là tuỳ chọn nâng cao |
| Phạm vi phình to (library, DSP, EQ…) | Chỉ làm những gì 0.3.0 đang có. Mọi thứ khác để sau P7 |
| Hết hứng giữa chừng | Nhờ P0 tách giao diện, dừng ở bất kỳ giai đoạn nào app vẫn chạy được với foobar2000 |

## 7. Câu hỏi cần trả lời trước khi bắt đầu

- DAC đang dùng là model gì? Hỗ trợ DoP hay native DSD? Có driver ASIO không?
- Thư viện nhạc có bao nhiêu file APE, WavPack, SACD ISO? Con số này quyết định có cần FFmpeg và xử lý ISO hay không.
- Có cần ReplayGain, EQ, crossfeed không? Mặc định là không, để giữ bit-perfect.
