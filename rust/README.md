# Desk Echo Guard（Rust 版）

Windows 用的全系統回聲消除程式，給用**桌上型喇叭 + 麥克風**（不戴耳機）講 Discord / LINE 的人。

- **回聲消除**：對方不會再從你的麥克風聽到自己的回音。
- **Hey Siri 防誤觸**（選用）：對方說「Hey Siri」時，比較不會叫醒你家的 HomePod / iPhone。

這是原本 Python 版的重寫：一樣的功能和設定檔，更省記憶體和 CPU、開得更快，介面改成 Windows 11 風格，設定改了就自動儲存、自動套用，滑鼠停在任何選項上都有說明。

> 狀態：重寫進行中，還沒有正式發佈。正式版請先用[原本的版本](../README.md)。

## 下載

到 [Releases](https://github.com/samfeng0517/desk-echo-guard/releases) 下載最新的 `DeskEchoGuard.exe`。不用安裝，雙擊就能開。

另外需要免費的 [VB-Cable](https://vb-audio.com/Cable/)（虛擬音效線），要自己安裝。

## 快速開始

1. 安裝 **VB-Cable**，重新開機。
2. 打開 `DeskEchoGuard.exe`，按 **開始**。
3. Discord / LINE 的麥克風選「**回聲消除麥克風**」，並關掉通話軟體自己的回音消除。

第一次用請照 **[5 分鐘上手](docs/getting-started.md)** 一步一步做。

## 文件

| 文件 | 給誰看 | 內容 |
|---|---|---|
| [5 分鐘上手](docs/getting-started.md) | 所有人 | 安裝、Discord / LINE 設定、確認有效、每天怎麼用 |
| [常見問題](docs/faq.md) | 遇到問題時 | 對方聽到回音、被吃字、聽不到、移除…… |
| [Hey Siri 防誤觸](docs/siri-guard.md) | 家裡有 HomePod / iPhone 的人 | 要不要開、怎麼設定、選哪個強度 |
| [運作原理](../docs/how-it-works.md) | 好奇的人 | 接線圖、時脈漂移、引擎比較、延遲 |
| [驗證結果與限制](../docs/verification.md) | 好奇的人 | 量測數據、已知限制 |
| [Hey Siri 研究與實測](../docs/siri-guard.md) | 好奇的人 | Apple 做法的研究、實測數據 |
| [設計與移植規格](DESIGN.md)、[介面規格](crates/app/UI.md) | 開發者 | crate 架構、API、GUI 規格 |

## 從原始碼建置（開發者）

需要 Rust 1.80 以上（[rustup](https://rustup.rs/)）。

```bash
cd rust
cargo build --release      # 產出 target/release/DeskEchoGuard.exe
cargo test --workspace --release
```

- **Windows**：需要 MSVC 工具鏈（Visual Studio Build Tools 的「使用 C++ 的桌面開發」）。
- **Linux**（只用來開發和跑測試，沒有 WASAPI 音效後端）：WebRTC 引擎用的 `libwebrtc` 預編譯檔需要 **clang 21**。編譯前先設定環境變數：`export CC=clang-21 CXX=clang++-21`。
- **Cargo features**（在 `echoguard-aec`）：
  - `webrtc`（預設開）：WebRTC AEC3 引擎，透過 LiveKit 的 `libwebrtc` crate。
  - `speex`（預設開）：SpeexDSP 引擎，內附 C 原始碼用 `cc` 編譯。
  - 例：只建 Speex 引擎 `cargo build -p echoguard-aec --no-default-features --features speex`。
- 自我檢測：`DeskEchoGuard.exe --self-test report.txt`，最後一行應是 `RESULT: PASS`。
- crate 結構、移植規則與 API 見 [DESIGN.md](DESIGN.md)。

## 授權

本專案以 [MIT 授權](../LICENSE)釋出。使用的第三方元件各自依原本的授權：

| 元件 | 用途 | 授權 |
|---|---|---|
| [Slint](https://slint.dev/) | 圖形介面 | [Slint Royalty-free Desktop, Mobile, and Web Applications License](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md)。本程式介面以 Slint 製作（Made with [Slint](https://slint.dev/)），「進階 → 關於」中附有 Slint 標示。 |
| [WebRTC](https://webrtc.org/)（經由 LiveKit `libwebrtc`） | 回聲消除、降噪 | BSD 3-Clause（WebRTC）；LiveKit Rust SDK 為 Apache-2.0 |
| [SpeexDSP](https://www.speex.org/) | 回聲消除（Speex 引擎） | BSD 3-Clause（Xiph.Org） |
| [openWakeWord](https://github.com/dscripka/openWakeWord) 特徵模型（執行時下載） | Hey Siri 喚醒詞偵測（detect 模式），用程式內建的推論器執行 | Apache-2.0 |
| [openWakeWord](https://github.com/dscripka/openWakeWord) | 喚醒詞偵測流程與特徵模型 | Apache-2.0（程式碼） |

[VB-Cable](https://vb-audio.com/Cable/) 是 VB-Audio 的軟體（VB-CABLE 免費、VB-CABLE A+B 為捐款軟體），需要另外安裝，不包含在本專案內。
