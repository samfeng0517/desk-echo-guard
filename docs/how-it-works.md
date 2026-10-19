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
 Discord/LINE 輸出 ──► CABLE-A Input ══► CABLE-A Output ──► 延遲 + 偵測 + 陷波 ──► 真正的喇叭
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
| **WebRTC**（預設） | WebRTC AEC3，透過 `livekit` | 全頻 48 kHz | **約 42 dB**（開降噪約 54 dB） | 會壓低你的聲音 | 一般使用 |
| **Speex** | SpeexDSP，透過 `pyaec` | 約 7 kHz | 約 23 dB（開降噪約 43 dB） | **保留得好** | 常同時講話、覺得被吃字 |
| **NLMS** | 內建純 numpy 自適應濾波器 | 約 7 kHz | 約 15–18 dB，沒有殘留回聲抑制 | 保留得好 | 前兩者都載入失敗時的備援 |

\* TTS 語音、40 ms 延遲的離線量測；完整數據見[驗證結果](verification.md#回聲消除)。

**雙講是什麼意思**

- 雙講 = 雙方同時講話。
- 量測時回聲和你的聲音差不多大聲。這時 WebRTC 輸出的清晰度（STOI）只剩 0.59，原始麥克風是 0.74；Speex 是 0.98。
- 回聲越小，WebRTC 壓低得越少。你的聲音比回聲大 14 dB 時是 0.88，原始麥克風是 0.96。
- WebRTC 引擎透過 livekit 呼叫，這方面的積極程度沒辦法調。

## 延遲

| 路徑 | 延遲 |
|---|---|
| 你的聲音（麥克風 → 對方） | 約 60–70 ms：輸出緩衝 40 ms + 10 ms 音框 + AEC3 約 9 ms |
| 對方聲音，Siri 防護 `always` | 約多 12.5 ms（濾波器群延遲） |
| 對方聲音，Siri 防護 `detect` / `sibilant` | 約多 1 秒（要等偵測器聽完整句） |

## 程式碼結構

```
echoguard/
  aec.py          回聲消除引擎（webrtc / speex / nlms）+ 延遲線
  buffers.py      ElasticBuffer（時脈漂移補償）、DelayLine
  resample.py     串流整數倍重取樣（48k ↔ 16k）
  delay.py        GCC-PHAT 延遲估計（「量測延遲」）
  siri_guard.py   線性相位陷波、偵測器、SiriGuard
  app.py          多執行緒即時管線
  io_backend.py   裝置 I/O 介面 + soundcard（WASAPI）實作
  sim.py          模擬裝置（測試用，可設各裝置的時脈誤差）
  signals.py / metrics.py   合成測試訊號、ERLE / STOI 等量測
  cli.py / gui.py           命令列 / 設定視窗
  selftest.py     免音效裝置的自我檢測（exe 的 --self-test）
  assets/         圖示
```

開發、測試、打包見 [開發說明](development.md)。
