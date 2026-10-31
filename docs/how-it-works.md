# 運作原理

[← 回 README](../README.md)

## 接線

```
                       ┌────────── 回聲消除 ───────────┐
 實體麥克風 ──────────► │ AEC（+ 降噪 / 高通）          │ ──► CABLE Input ══► CABLE Output ──► Discord/LINE 麥克風
                       │        ▲ 回聲參考              │
 喇叭 loopback ────────► │────────┘ （彈性緩衝 + 漂移補償）│
                       └──────────────────────────────┘

 （選用：Hey Siri 防護）
 Discord/LINE 輸出 ──► CABLE-A Input ══► CABLE-A Output ──► （預看 +）偵測 + 陷波 ──► 真正的喇叭
```

**回聲參考**

- 取自真正喇叭的 loopback，也就是 Windows 實際送到喇叭的聲音。
- 這已經是 Siri 防護處理之後的版本，連其他程式（YouTube、遊戲）的聲音也包在裡面，正好就是麥克風會收到的回聲來源，所以全部都能一起消掉。

**時脈漂移**

- 每個音效裝置都有自己的時脈，彼此會差幾十到幾百 ppm。放著不管的話，參考訊號會慢慢和麥克風錯開。
- 裝置之間用「彈性緩衝」連接：
  - 用推入時間戳內插出連續的水位，再用臨界阻尼 PI 控制器微調重取樣比例。
  - 測試中 ±500 ppm 都追得到，估計誤差小於 1 ppm。
- 欠載時讀取位置照樣前進，晚到的樣本直接跳過，所以參考和麥克風的對齊不會亂掉。

## 引擎比較

| 引擎 | 實作 | 頻寬 | 遠端單講時的回聲衰減* | 雙方同時講話 | 適合 |
|---|---|---|---|---|---|
| **WebRTC**（預設） | WebRTC AEC3，透過 LiveKit 的 `libwebrtc` crate | 全頻 48 kHz | **約 42 dB**（開降噪約 54 dB） | 會壓低你的聲音 | 一般使用 |
| **Speex** | SpeexDSP（內附 C 原始碼，一起編進 exe） | 約 7 kHz | 約 23 dB（開降噪約 43 dB） | **保留得好** | 常同時講話、覺得被吃字 |
| **NLMS** | 內建的純 Rust 自適應濾波器 | 約 7 kHz | 約 15–18 dB，沒有殘留回聲抑制 | 保留得好 | 前兩者都載入失敗時的備援 |

\* TTS 語音、40 ms 延遲的離線量測（Python 版 v0.3.1 的數字；Rust 版的測試數字也列在驗證結果裡）；完整數據見[驗證結果](verification.md#回聲消除)。

**雙講是什麼意思**

- 雙講 = 雙方同時講話。
- 量測時回聲和你的聲音差不多大聲。這時 WebRTC 輸出的清晰度（STOI）只剩 0.59，原始麥克風是 0.74；Speex 是 0.98。
- 回聲越小，WebRTC 壓低得越少。你的聲音比回聲大 14 dB 時是 0.88，原始麥克風是 0.96。
- WebRTC 引擎透過 LiveKit 的 `libwebrtc` 呼叫，這方面的積極程度沒辦法調。

## 延遲

| 路徑 | 延遲 |
|---|---|
| 你的聲音（麥克風 → 對方） | 約 60–70 ms：輸出緩衝 40 ms + 10 ms 音框 + AEC3 約 9 ms |
| 對方聲音，Siri 防護 `always` | 約多 12.5 ms（濾波器群延遲） |
| 對方聲音，Siri 防護 `sibilant` | 約多 0.07 秒（濾波器 12.5 ms + 預看 60 ms：偵測到「s」音時要回頭從它開頭削起） |

## 音效裝置

- 用 Windows 的 WASAPI 共用模式，喇叭聲音用 loopback 擷取。
- 每個裝置一條**事件驅動**的執行緒：Windows 通知有資料才讀寫，不輪詢（舊的 Python 版用 `soundcard` 輪詢）。
- 所有處理聲音的執行緒（裝置執行緒和回聲消除運算）都用 Windows 的 MMCSS「Pro Audio」優先權，程式也關掉自己的電源節流（EcoQoS）；視窗縮小或縮到系統匣時，Windows 不再優先照顧這個程式，沒有這些設定就可能來不及處理而影響效果。
- 多聲道裝置會混成單聲道；喇叭沒在播聲音、Windows 不送資料時，照樣補上靜音，參考訊號不會斷。
- 程式內部一律是 48 kHz 單聲道、10 ms（480 樣本）一個音框；Speex / NLMS 在 16 kHz 處理。

## 程式碼結構

Rust workspace，`crates/` 底下五個 crate（細節見 [DESIGN.md](../DESIGN.md)）：

```
crates/
  dsp/        純 DSP，不碰作業系統：ElasticBuffer（時脈漂移補償）、DelayLine、
              串流整數倍重取樣（48k ↔ 16k）、GCC-PHAT 延遲估計（「量測延遲」）、
              線性相位陷波與 SiriGuard、NLMS、合成測試訊號與 ERLE / STOI 等量測
  aec/        回聲消除引擎（webrtc / speex / nlms / off）+ 延遲線（AecProcessor）
  audio/      裝置介面、WASAPI 實作（Windows）、模擬裝置（測試用，可設各裝置的時脈誤差）
  core/       設定檔、多執行緒即時管線、量測延遲、VB-Cable 改名 / 停用、
              開機自動啟動、自我檢測（--self-test）
  app/        DeskEchoGuard.exe：Slint 圖形介面（ui/、規格見 UI.md）、系統匣、命令列
assets/       圖示（tools/make_icon.py 產生）
tools/        圖示產生器（Python）
```

開發、測試、發佈見 [開發說明](development.md)。
