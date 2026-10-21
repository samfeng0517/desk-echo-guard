# 開發說明

[← 回 README](../README.md)

程式是 Rust workspace，放在專案根目錄（`Cargo.toml`、`crates/`）。crate 結構、API 與移植規則見 [DESIGN.md](../DESIGN.md)，介面規格見 [crates/app/UI.md](../crates/app/UI.md)。舊的 Python 版已移除，原始碼見 [v0.3.1 標籤](https://github.com/samfeng0517/desk-echo-guard/tree/v0.3.1)。

## 編譯與測試

需要 Rust 1.92 以上（[rustup](https://rustup.rs/)）。

```bash
cargo build --release -p desk-echo-guard          # target/release/DeskEchoGuard(.exe)
cargo test --workspace --release                  # 全部測試
cargo test --workspace --release -- --nocapture   # 印出每項的量測數字（ERLE 等）
cargo clippy --workspace --all-targets --release -- -D warnings
```

- 測試用 `--release` 比較快：DSP 沒最佳化會很慢（`Cargo.toml` 已讓相依套件在 debug / test 也最佳化）。
- 單元測試在各 crate 的原始碼旁邊，整合測試在 `crates/*/tests/`，例如：

| 測試 | 內容 |
|---|---|
| `crates/aec/tests/test_aec.rs` | 各引擎 ERLE、近端保留度、雙講 |
| `crates/core/tests/pipeline_sim.rs` | 完整多執行緒管線接模擬裝置（各裝置時脈不同） |
| `crates/core/tests/pipeline_leaks.rs` | 開始 / 停止 20 次不留執行緒、記憶體不增加（只在 Linux） |
| `crates/core/tests/selftest.rs` | 自我檢測 |
| `crates/app/tests/cli_sim.rs` | 命令列、離線 `--sim-*` |
| `crates/wakeword/tests/reference.rs` | 喚醒詞偵測分數和 Python openWakeWord 的參考輸出比對（要下載模型；離線時跳過，或設 `ECHOGUARD_OWW_OFFLINE` 跳過） |

### Windows

- 需要 MSVC 工具鏈（Visual Studio Build Tools 的「使用 C++ 的桌面開發」）。
- `.cargo/config.toml` 設定了**靜態 CRT**（`+crt-static`）：LiveKit 預編譯的 `libwebrtc` 用 `/MT` 編譯，其他部分必須一致，否則連結失敗。好處是 exe 不需要 VC++ 執行階段 DLL。

### Linux（只用來開發和跑測試）

- 沒有 WASAPI 音效後端，只有模擬裝置；可以跑全部測試和 `--screenshot`。
- WebRTC 引擎用的 `libwebrtc` 預編譯檔需要 **clang 21**，並且要設定環境變數（`webrtc-sys` 只讀 `CC` / `CXX`）：

  ```bash
  export CC=clang-21 CXX=clang++-21
  ```

- 還需要介面相關的開發套件（Ubuntu 24.04）：

  ```bash
  sudo apt-get install clang-21 libfontconfig1-dev libxkbcommon-dev libglib2.0-dev
  ```

  clang 21 來自 <https://apt.llvm.org/>，安裝方式見 `.github/workflows/ci.yml`。

### Cargo features（`echoguard-aec`）

- `webrtc`（預設開）：WebRTC AEC3 引擎，透過 LiveKit 的 `libwebrtc` crate。
- `speex`（預設開）：SpeexDSP 引擎，內附 C 原始碼（`crates/aec/vendor`）用 `cc` 編譯。
- 例：只建 Speex 引擎 `cargo build -p echoguard-aec --no-default-features --features speex`。

## CI

工作流程在 `.github/workflows/`：

| 檔案 | 什麼時候跑 | 做什麼 |
|---|---|---|
| `ci.yml` | Pull request、推到 `main` 以外的分支、手動 | Windows：clippy、全部測試、編譯 exe、`--self-test`，exe 和報告上傳到 Artifacts。Linux：全部測試 |
| `build-exe.yml` | 推到 `main`、推 `v*` 標籤、手動 | Windows：全部測試、編譯 exe、`--self-test`，上傳到 Artifacts；標籤或手動填 `release_tag` 時發佈 GitHub Release |
| `delete-release.yml` | 手動 | 刪除一個 Release 和它的標籤 |

## 發佈新版本

1. 把 `Cargo.toml` 的 `[workspace.package]` 裡的 `version` 改成新版本號（例如 `0.4.1`），合併到 `main`。
2. 擇一：
   - 推一個 `v*` 標籤（例如 `v0.4.1`）。
   - 在 Actions 頁面手動執行「Build Windows exe」，`release_tag` 填版本號。
3. 兩種方法都會建立 GitHub Release，並附上 `DeskEchoGuard.exe`。Release 說明的開頭文字寫在 `build-exe.yml` 裡。

**刪除某個版本**：手動執行「Delete release」並填標籤名稱，Release 和標籤會一起刪掉。

> `rust/docs/` 裡的三個檔案是轉址頁：v0.4.0 的 exe 內的連結指向那裡，請保留。

## 文件截圖

```bash
cargo run --release -p desk-echo-guard -- --screenshot docs/images
```

- 用模擬引擎（不需要音效裝置）、軟體繪圖，把每一頁的淺色 / 深色、提示框、InfoBar、對話框範例存成 PNG。
- Linux 和 Windows 都能跑。

## 圖示

修改 `tools/make_icon.py` 後執行它（需要 Python 和 Pillow），會重新產生 `assets/icon.ico` 和 `assets/icon.png`。`icon.ico` 會在編譯時嵌進 exe。

## Hey Siri 喚醒詞模型

訓練 `detect` 模式用的模型是另外的 Python 流程，見 [tools/train_hey_siri](../tools/train_hey_siri/README.md)。
