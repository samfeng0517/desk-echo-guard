# Hey Siri 防誤觸：研究與實測

[← 回 README](../README.md) ・ [Hey Siri 防誤觸（怎麼設定）](siri-guard.md)

這一頁是給好奇的人看的研究筆記和量測數據；只想知道怎麼開、怎麼選，看 [Hey Siri 防誤觸](siri-guard.md) 就好。

要解決的問題：通話對方說「Hey Siri」，從你的喇叭播出來，把你的 HomePod / iPhone 叫醒。

做法是仿照 Apple 在 WWDC 2026 發表會被觀察到的手法：**不是靜音**，而是只在講到「Siri」的那一小段，削掉幾個窄頻帶。人聽得懂，偵測器比較認不出來。

接線上，Discord / LINE 的輸出改到 `CABLE-A Input`，程式從 `CABLE-A Output` 收到通話聲音，處理完再播到喇叭（見[運作原理](how-it-works.md#接線)）。

## 怎麼選

**陷波強度（profile）**

| 設定 | 削掉的頻帶 | 深度 | 聽感 |
|---|---|---|---|
| `deep-wide`（預設） | 2.8–6.5 kHz 整段 | 70 dB | 變暗；子音也在這段，實際通話有人覺得對方講話不好聽懂（那時改 `apple-deep`） |
| `apple-deep` | 3/4/5/6 kHz，各寬 400 Hz | 70 dB | 幾乎聽不出來 |
| `apple-wide` | 2.8–6.5 kHz | 50 dB | 變暗，子音較不清楚 |
| `formant` | 1–3 kHz + 上述四個窄帶 | 70 dB | 明顯變悶，但仍聽得懂 |

**模式**

| 模式 | 什麼時候處理 | 通話多出的延遲 | 需要 |
|---|---|---|---|
| `always`（預設） | 一律處理 | 約 12.5 ms | — |
| `sibilant` | 偵測到 /s/ 音（Siri 的開頭）後處理 0.45 s，其他含 s 的字也會被處理到 | 約 72.5 ms（v0.4.1 前約 1 秒） | — |

v0.4.1 移除了 `detect` 模式（openWakeWord 喚醒詞模型偵測到「Hey Siri」才處理那一句）：它需要自己訓練「Hey Siri」模型，作者沒有訓練出可用的模型，又讓對方聲音晚約 1 秒，不值得維護。舊設定檔的 `siri_mode = "detect"` 會自動改成 `sibilant`，`siri_model` / `siri_threshold` / `siri_lookahead_s` 會被忽略。下面的研究數據仍保留當時的結果。

**建議**

1. 用預設的 `deep-wide` + `always`。它涵蓋 `apple-deep` 的四個頻帶，再把中間的空隙也一起削掉。可以拿另一台裝置播「Hey Siri」影片，確認 HomePod / iPhone 不會被叫醒。
   - 舊版的 `apple`（同 `apple-deep` 的頻帶、只削 30 dB）實機擋不住，已經移除；設定檔裡還寫著 `apple` 的話，會自動改用 `deep-wide`。
   - `deep-wide` 削掉的 2.8–6.5 kHz 也是子音所在的頻段；對方講話變得不好聽懂時，改用 `apple-deep`。
2. 還會被叫醒的話，改用 `formant`。`formant` + `always` 會讓整通電話都變悶，所以最好搭配 `sibilant` 模式：只有「s」音後約半秒變悶，其他時間不受影響，延遲也只多約 0.06 秒。

為什麼這樣建議，見下方[實測效果](#實測效果)。

## Apple 怎麼做：研究摘要

### WWDC 2026 主題演講的頻譜分析

- 荷蘭音訊工程師 Luuk de Leest 在 X 上貼出頻譜圖：每次講到「Siri」時，**約 3、4、5、6 kHz 四個窄頻帶被削掉**，其他時間的音軌沒有動。
- 報導稱之為「**動態陷波濾波器**」：只在講觸發詞的瞬間啟動。
- 觀眾仍聽得懂，只是稍微變薄、有點「金屬感」。
- **Apple 沒有官方證實。**
- **效果不完全**：多名觀眾回報裝置還是被叫醒。
- **公開資料沒有**：陷波的寬度、深度（dB）、起落時間。
- 來源：
  - [MacRumors](https://www.macrumors.com/2026/06/12/apple-cut-frequencies-to-prevent-siri-activations/)
  - [heise online](https://www.heise.de/en/news/WWDC-2026-Apple-filtered-frequencies-to-prevent-Siri-activations-11333194.html)
  - [TechRadar](https://www.techradar.com/ai-platforms-assistants/an-apple-siri-mystery-has-been-solved-by-an-audio-engineer-spectrogram-reveals-how-apple-stopped-voice-assistant-setting-off-millions-of-iphones-and-homepods-during-its-wwdc-2026-event)
  - [Wccftech](https://wccftech.com/apple-modified-the-frequencies-of-its-wwdc-2026-keynote-to-avoid-triggering-your-devices-each-time-siri-was-mentioned/)
  - [Stuff](https://www.stuff.tv/news/well-isnt-this-neat-apple-modified-the-way-siri-was-spoken-during-its-keynote-to-stop-your-iphone-from-listening/)
  - 原始貼文：[@luuk58](https://x.com/luuk58/status/2064085109980987720)、[tech.explain](https://x.com/techexplain1/status/2069494958768504836)

### Apple 的 Hey Siri 偵測器

資料來源：[Apple Machine Learning Research, 2017](https://machinelearning.apple.com/research/hey-siri)

- 16 kHz 取樣，每 10 ms 一個 mel 頻譜框，約 0.2 秒的視窗送進 DNN。
- 兩段式：先在低功耗處理器跑小模型，再由主處理器的大模型確認。
- 有「說話者辨識」，會比對註冊時錄的 5 句。所以別人說「Hey Siri」**本來就不一定**叫得醒你的裝置。
- 文章沒有說明任何針對電視或媒體聲音的防護。相關專利 [US20140222436A1](https://patents.google.com/patent/US20140222436A1/en) 也沒有。

### 其他公司的做法

- **Amazon**
  - 2017 年 Alexa 廣告的 3–6 kHz 被網友發現大幅衰減，但 Amazon 沒有證實。
  - 官方說法是**聲紋指紋**：事先把廣告做指紋，放到 Echo 上比對後忽略；也會即時偵測很多家庭同時聽到同一句指令。
  - 來源：[Amazon Science](https://amazon.science/blog/why-alexa-wont-wake-up-when-she-hears-her-name-in-amazons-super-bowl-ad)、[Popular Science](https://www.popsci.com/why-amazon-alexa-can-ignore-television/)
- **Google**：有「錄製媒體熱詞抑制」專利（[US10867600B2](https://patents.google.com/patent/US10867600)），同樣靠指紋比對。
- 指紋比對要**裝置端配合**，沒辦法對你的 HomePod 用。所以在聲音訊號這一端能做的，只剩修改頻譜這條路。

## 實作方式

程式碼在 `crates/dsp/src/notch.rs`（陷波濾波器）和 `crates/dsp/src/guard.rs`（SiriGuard、`sibilant` 偵測、時間控制）。v0.4.0 的 `crates/wakeword`（`detect` 模式的 openWakeWord 偵測器）已在 v0.4.1 移除。

**陷波濾波器**

- 線性相位 FIR 多頻帶阻帶（1201 taps，群延遲 12.5 ms）。
- 處理後的聲音和延遲後的原音逐樣本對齊，所以能用 `out = dry − m(t)·(dry − wet)` 平滑切換（預設 15 ms），不會有梳狀濾波的怪聲。

**`sibilant` 模式的時間控制**

- 偵測器跑在**還沒延遲**的輸入上，通話聲音則延遲一小段（`lookahead`）。
- 偵測器每 10 ms 一框；/s/ 開頭的那一框結束時觸發，標記的區段是「該框開頭往前 30 ms」到「往後 0.45 s」。
- 所以 `lookahead` 只要涵蓋：框長 10 ms + 往前的 30 ms + 淡入 15 ms + 5 ms 餘裕 = 60 ms（由偵測器參數算出，和音訊區塊大小無關）。加上濾波器 12.5 ms，總延遲約 72.5 ms。這樣 /s/ 一開頭就已經被削，不會漏掉開頭。
- 舊的 `detect` 模式要等整句講完才觸發，所以需要約 1 秒的 `lookahead`。

## 實測效果

拿不到 Siri 的偵測器，所以改用 openWakeWord 的開源模型（alexa、hey_jarvis、hey_mycroft）當替身，對 Piper TTS 合成的女聲和男聲做測試。

- 數字是偵測分數（0–1，≥0.5 算觸發）；括號是 STOI 清晰度（1 = 完全不變）。
- 下表是用 Python 版（v0.3.1）的評估腳本 `experiments/guard_proxy_eval.py` 量的（openWakeWord 0.6.0 的 ONNX 模型）。這些腳本已隨 Python 版移除，要重現請到 [v0.3.1 的原始碼](https://github.com/samfeng0517/desk-echo-guard/tree/v0.3.1/experiments)。Rust 版的陷波濾波器照同樣的設計移植，並通過同樣門檻的測試。
- 想自己聽聽處理後的聲音：`DeskEchoGuard.exe --sim-guard-in 通話.wav --sim-guard-out 處理後.wav --siri-mode always --siri-profile formant`（見[進階使用](usage.md#離線處理錄音)）。

| 片語 / 聲音 | 原始 | deep-wide | apple-deep | apple-wide | formant |
|---|---|---|---|---|---|
| alexa / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.97) | 1.00 (0.98) | **0.00** (0.92) |
| alexa / 男聲 | 1.00 | 1.00 (0.98) | 1.00 (0.99) | 1.00 (0.98) | **0.00** (0.94) |
| hey_jarvis / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.98) | 1.00 (0.98) | 0.66 (0.94) |
| hey_jarvis / 男聲 | 1.00 | 1.00 (0.99) | 1.00 (0.99) | 1.00 (0.99) | **0.00** (0.95) |
| hey_mycroft / 女聲 | 0.00 | 0.00 (0.97) | 0.00 (0.98) | 0.00 (0.98) | 0.00 (0.95) |
| hey_mycroft / 男聲 | 0.99 | **0.01** (0.99) | 0.94 (0.99) | **0.02** (0.99) | **0.00** (0.93) |

- hey_mycroft / 女聲在原始聲音就沒有觸發，這一列不能拿來比較。
- 「Hey Siri」合成語音處理後的 STOI：`deep-wide` 0.98–0.99、`apple-deep` 0.98–0.99、`formant` 0.93–0.94。

**`detect` 模式端到端測試**（Python 版；此模式已在 v0.4.1 移除）：用 hey_jarvis 當替身，喚醒詞放在句子中間。

- 偵測器正確觸發，只處理了那一段（約 1.0 s），其他部分和原音完全相同。
- 處理後的聲音再交給另一個偵測器：舊版 30 dB 的 `apple` 分數仍是 1.00；`formant` 降到 0.00–0.04。

**結論**

- **Apple 式的 3/4/5/6 kHz 窄頻陷波，對這三個開源偵測器幾乎沒效。** 它們主要依賴 3 kHz 以下的共振峰資訊，訓練時也做了大量資料增強。
- 把整段 2.8–6.5 kHz 削掉（`deep-wide`、`apple-wide`）只多擋下一組（hey_mycroft / 男聲），其他照樣觸發。對開源偵測器來說，真正有效的還是削到 3 kHz 以下的 `formant`。
- 這和 WWDC 觀眾「還是被叫醒」的回報一致。
- Siri 的偵測器是否特別依賴 3–6 kHz 的 /s/ 音，**沒有公開證據**；唯一的間接線索是 Apple 自己選了這幾個頻帶。

STOI 只看約 4.3 kHz 以下的頻段，所以對高頻陷波不太敏感。合成語音上量到 `deep-wide` 的目標頻帶衰減 ≥ 45 dB，2.5 kHz 以下的頻譜不變（LSD 0.00 dB），總能量只少 0.23 dB。其他驗證數據見[驗證結果](verification.md#hey-siri-陷波)。

## 限制

- Apple 的方法沒有公開。實機上 30 dB 的淺陷波擋不住 HomePod / iPhone；`deep-wide` 在實機上的效果**還沒實測**。
- `sibilant` 會處理到其他含 s 的字（每次約半秒）。
- `sibilant` 只在 /s/ 之後削；如果 Siri 的偵測器在 /s/ 之前（「Hey」）就已經認出來，這個模式擋不到。
- `sibilant` 的延遲約 72.5 ms（`always` 約 12.5 ms），對話時感覺不出來。
