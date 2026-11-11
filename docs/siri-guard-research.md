# Hey Siri 防誤觸：研究與實測

[← 回 README](../README.md) ・ [Hey Siri 防誤觸（怎麼設定）](siri-guard.md)

這一頁是給好奇的人看的研究筆記和量測數據；只想知道怎麼開、怎麼選，看 [Hey Siri 防誤觸](siri-guard.md) 就好。

要解決的問題：通話對方說「Hey Siri」，從你的喇叭播出來，把你的 HomePod / iPhone 叫醒。

做法是從 Apple 在 WWDC 2026 發表會被觀察到的手法出發：**不是靜音**，而是削掉一部分頻帶。人聽得懂，偵測器比較認不出來。Apple 只削 3–6 kHz 的幾個窄頻帶；實測那樣擋不住，所以本程式削得更寬（見下方）。

接線上，Discord / LINE 的輸出改到 `CABLE-A Input`，程式從 `CABLE-A Output` 收到通話聲音，處理完再播到喇叭（見[運作原理](how-it-works.md#接線)）。

## 怎麼選

**陷波強度（profile）**

| 設定 | 削掉的頻帶 | 深度 | 聽感 |
|---|---|---|---|
| `deep-wide`（標準，預設） | 1.0–6.5 kHz 整段 | 70 dB | 明顯變悶，聽得懂 |
| `formant`（加強） | 0.5–8 kHz 整段 | 70 dB | 幾乎只剩低音，很難聽懂；搭配 `heysiri` / `sibilant` 用 |

- v0.4.5 以前的 `deep-wide` 是 2.8–6.5 kHz，`formant` 是 1–3 kHz 加 3/4/5/6 kHz 四個窄帶。實機用 `always` + 舊 `deep-wide` 仍會被叫醒，所以 v0.4.6 把兩者都加強（[依據](#要削哪一段v046-的強度是怎麼定的)）。
- 比舊 `deep-wide` 還弱的 `apple-deep`（3/4/5/6 kHz 窄帶，70 dB）、`apple-wide`（2.8–6.5 kHz，50 dB）、`apple`（窄帶，30 dB）都已移除，設定檔裡的舊名稱會自動改用 `deep-wide`。

**模式**

| 模式 | 什麼時候處理 | 通話多出的延遲 |
|---|---|---|
| `always`（預設） | 一律處理 | 約 12.5 ms |
| `heysiri`（v0.4.6） | /s/ 音開頭、而且前面像「Hey」才處理；之後不像「iri」就在 0.13–0.26 s 內停止，否則處理 0.45 s | 約 72.5 ms |
| `sibilant` | 偵測到 /s/ 音後處理 0.45 s，其他含 s 的字也會被處理到 | 約 72.5 ms |

v0.4.1 移除了 `detect` 模式（openWakeWord 喚醒詞模型偵測到整句「Hey Siri」才處理）：整句講完才觸發，所以對方聲音要晚約 1 秒，而且沒有可用的「Hey Siri」模型。`heysiri` 模式改成「聽到開頭就先處理、不像再放開」，不需要多等。舊設定檔的 `siri_mode = "detect"` 會自動改成 `sibilant`，`siri_model` / `siri_threshold` / `siri_lookahead_s` 會被忽略。

**建議**

1. 先用預設的 `always` + `deep-wide`，拿另一台裝置播「Hey Siri」影片，確認 HomePod / iPhone 不會被叫醒。
2. 覺得整通電話都悶：模式改 `heysiri`，只有「Hey Siri」那半秒會被處理。
3. 還會被叫醒：強度改 `formant`（搭配 `heysiri` 或 `sibilant`）。`heysiri` 漏掉太多次的話改 `sibilant`。

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

### 其他調查到的資料（2026-11）

- **Apple 的陷波效果有限**：MacRumors 論壇上有多位 iPhone 使用者回報發表會中被叫醒 3–5 次，HomePod 和 Apple Watch 使用者則說沒被叫醒（[討論串](https://forums.macrumors.com/threads/apple-cut-frequencies-in-wwdc-keynote-to-prevent-siri-activations.2483884/)）。沒有人公開重現過這個濾波器並在實機上量測。
- **Apple 另有指紋比對的機制**：tvOS 18.2 beta 裡有一個「AdBlocker」框架，會下載音訊指紋、用 ShazamKit 比對麥克風聽到的聲音，暫時停用 Siri 觸發（[9to5Mac](https://9to5mac.com/2024/11/07/apple-tv-ads-hey-siri/)）。這需要事先知道內容，通話用不上。
- **陷波是「標記」還是「干擾」？** 2017 年 Reddit 上有人發現 Alexa 廣告在 3–6 kHz 被削，並猜測 Echo 是把這個缺口當成「這是廣告，不要理」的標記（[The Next Web](https://thenextweb.com/news/heres-why-alexa-wont-light-up-during-amazons-super-bowl-ad)）。Amazon 和 Apple 都沒有證實裝置會檢查頻譜缺口；發表會上還是有 iPhone 被叫醒，比較不像是硬性的標記。Amazon 後來公開的做法是聲紋指紋和展頻浮水印（[Amazon Science](https://amazon.science/blog/audio-watermarking-algorithm-is-first-to-solve-second-screen-problem-in-real-time)、專利 [US10950249](https://patents.google.com/patent/US10950249)）。
- **Siri 偵測器的公開資料**：16 kHz 取樣、mel 濾波器組（[2017](https://machinelearning.apple.com/research/hey-siri)）；2023 年的架構是 DNN-HMM 第一關（7 個音素、21 個狀態）→ conformer 檢查器 → 說話者辨識 → 整句的誤觸發抑制（[Voice Trigger System for Siri](https://machinelearning.apple.com/research/voice-trigger)）。差一點觸發時，裝置會在接下來幾秒變得比較敏感，所以「擋下一半、對方再說一次」仍可能觸發。
- **只說「Siri」就能喚醒**目前只支援英文（美、英、加、澳）、法文、德文和中國大陸的普通話；台灣的國語和粵語不在清單裡，仍然要說「嘿 Siri」（[Apple 功能支援清單](https://www.apple.com/ios/feature-availability/)）。這是 `heysiri` 模式只認「Hey + Siri」的依據。
- **對抗式擾動**：「Adversarial Music」（[NeurIPS 2019](https://arxiv.org/abs/1911.00126)）要先仿造出 Alexa 的模型才做得到；沒有找到能轉移到 Siri、又能撐過喇叭播放的通用擾動。即時的語音偽裝研究（[VoiceBlock](https://interactiveaudiolab.github.io/project/voiceblock.html)、[Neural Voice Camouflage](https://voicecamo.cs.columbia.edu/)）針對的是說話者辨識或語音辨識，不是喚醒詞。
- **干擾訊號**：有研究指出只要 2 ms、時間點精準的干擾訊號就能讓喚醒詞偵測失效（[UCC, 2024](https://cora.ucc.ie/items/d7c76bc1-68cf-4273-9453-7528e0b34909)），但測了哪些偵測器不清楚，也沒有 Siri 的資料。這是之後可以試的方向（在「Siri」中間插入很短的靜音或雜訊）。
- **現成的開放詞彙關鍵詞偵測**：sherpa-onnx 的 KWS 模型可以直接用文字指定關鍵詞、不用訓練，但判定延遲是 160–320 ms 加上整個詞的長度（[文件](https://k2-fsa.github.io/sherpa/onnx/kws/pretrained_models/index.html)），而且要帶 ONNX Runtime。沒有找到公開的「Hey Siri」預訓練模型，也沒有公開可用的真人「Hey Siri」錄音。

## 實作方式

程式碼在 `crates/dsp/src/notch.rs`（陷波濾波器）、`crates/dsp/src/guard.rs`（SiriGuard、`sibilant` 偵測、時間控制）和 `crates/dsp/src/heysiri.rs`（`heysiri` 偵測）。v0.4.0 的 `crates/wakeword`（`detect` 模式的 openWakeWord 偵測器）已在 v0.4.1 移除。

**陷波濾波器**

- 線性相位 FIR 多頻帶阻帶（1201 taps，群延遲 12.5 ms）。
- 處理後的聲音和延遲後的原音逐樣本對齊，所以能用 `out = dry − m(t)·(dry − wet)` 平滑切換（預設 15 ms），不會有梳狀濾波的怪聲。

**`sibilant` / `heysiri` 模式的時間控制**

- 偵測器跑在**還沒延遲**的輸入上，通話聲音則延遲一小段（`lookahead`）。
- 偵測器每 10 ms 一框；/s/ 開頭的那一框結束時觸發，標記的區段從「該框開頭往前 30 ms」開始。
- 所以 `lookahead` 只要涵蓋：框長 10 ms + 往前的 30 ms + 淡入 15 ms + 5 ms 餘裕 = 60 ms（由偵測器參數算出，和音訊區塊大小無關）。加上濾波器 12.5 ms，總延遲約 72.5 ms。這樣 /s/ 一開頭就已經被削，不會漏掉開頭。
- `sibilant`：每個 /s/ 開頭都標記 0.45 s。
- `heysiri`：見下一節。

### `heysiri` 模式的偵測器

程式碼在 `crates/dsp/src/heysiri.rs`，訓練腳本在 [`tools/heysiri`](../tools/heysiri/README.md)。

- 整句「Hey Siri」要講完才認得出來，等它講完再處理就要延遲約 1 秒（舊的 `detect` 模式）。所以把判斷拆成三關，都以 /s/ 的開頭為基準：
  - **第 0 關（/s/ 開頭當下）**：前 0.4 秒像不像「Hey」加上 /s/ 的開頭？像就先開始處理 0.13 秒。「Hey」已經講完了，所以不用多等。
  - **第 1 關（0.12 秒後）**、**第 2 關（0.25 秒後）**：聽起來還像「Hey Si…」「Hey Sir…」嗎？像就把處理區段延長（到 0.26 秒、再到 0.45 秒），不像就讓它結束。
- 每一關是一個小 CNN（32 個 mel 頻帶、10 ms 一框；兩層 1D 卷積加兩層全連接，約 1.6–2.2 萬個參數），三關合計約 230 KB，直接編進 exe，用純 Rust 計算，不需要 ONNX Runtime。
- /s/ 的判斷比 `sibilant` 模式寬鬆（小聲、有雜訊的 /s/ 也算），多出來的候選交給模型篩掉。

**訓練資料**（全部公開）

- 正例：Piper 語音合成念的「Hey Siri」（約 1,060 位英文語者，含非母語者），以及 [TigreGotico/synthetic-wakeword-hey_siri](https://huggingface.co/datasets/TigreGotico/synthetic-wakeword-hey_siri)（其他合成引擎，1,288 段）。**沒有真人錄音。**
- 反例：真人朗讀的中文、粵語、英文（[FLEURS](https://huggingface.co/datasets/google/fleurs)），合成的中文句子，以及「Hey Sam」「Hey sister」「黑色」「沒事」這類開頭相近的詞（給第 1、2 關學會放開）。
- 一部分加上噪音、窄頻、Opus 編碼來模擬通話。

**測試結果**（訓練時沒看過的語者／語音／錄音；門檻用另一份驗證資料決定）

| 測試資料 | 結果 |
|---|---|
| 「Hey Siri」：Piper `libritts_r` 沒看過的語者（423 段） | 99.1% 完整處理 |
| 「Hey Siri」：Piper `l2arctic` 非母語者（120 段） | 97.5% |
| 「Hey Siri」：Piper `vctk` 英國腔（128 段） | 96.1% |
| 「Hey Siri」：Tigre `original`（504 段） | 95.4% |
| 「Hey Siri」：Amazon Polly 沒看過的 5 種聲音（190 段） | 84.7% |
| 「Hey Siri」：Tigre `legacy-tts`（60 段，來源不明的舊式合成引擎，訓練完全沒看過） | 1.7% |
| 以上合計（1,685 段） | 92.0%（其中 1.8% 是 /s/ 本身沒被偵測到） |
| 真人中文朗讀（FLEURS 測試集，385 分鐘，18,937 個 /s/ 類開頭） | 9.9% 通過第 0 關（處理 0.13 秒就放開），0.3% 被完整處理；**處理時間占 1.4%**（`sibilant` 模式約 19%） |
| 「Hey Sam」這類「Hey + 其他 s 開頭的字」（804 段） | 88% 通過第 0 關（預期如此），19% 被完整處理 |

- 出貨的模型是把測試資料也加進去再訓練一次的版本；在驗證資料上「Hey Siri」96.1% 被完整處理，其他語音的處理時間占 0.9%。
- **怎麼解讀：** 對合成語音大約 9 成以上有效，但 `legacy-tts` 那一列說明模型遇到差異很大的聲音會完全認不出來。真人的聲音落在哪一邊，要實機試才知道。漏掉時不會有任何處理，所以在意擋住率的話用 `always`。
- 中文腔調說的「嘿 Siri」沒有專門的測試資料。合成中文語音念「嘿，四個」「黑色」這類詞時，約一半會通過第 0 關，表示中文的「嘿」多半認得出來，但沒有直接量過。

## 實測效果

拿不到 Siri 的偵測器，所以改用 openWakeWord 的開源模型（alexa、hey_jarvis、hey_mycroft）當替身，對 Piper TTS 合成的女聲和男聲做測試。

- 數字是偵測分數（0–1，≥0.5 算觸發）；括號是 STOI 清晰度（1 = 完全不變）。
- 下表是用 Python 版（v0.3.1）的評估腳本 `experiments/guard_proxy_eval.py` 量的（openWakeWord 0.6.0 的 ONNX 模型）。這些腳本已隨 Python 版移除，要重現請到 [v0.3.1 的原始碼](https://github.com/samfeng0517/desk-echo-guard/tree/v0.3.1/experiments)。Rust 版的陷波濾波器照同樣的設計移植，並通過同樣門檻的測試。
- 想自己聽聽處理後的聲音：`DeskEchoGuard.exe --sim-guard-in 通話.wav --sim-guard-out 處理後.wav --siri-mode heysiri --siri-profile deep-wide`（見[進階使用](usage.md#離線處理錄音)）。

| 片語 / 聲音 | 原始 | deep-wide | apple-deep | apple-wide | formant |
|---|---|---|---|---|---|
| alexa / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.97) | 1.00 (0.98) | **0.00** (0.92) |
| alexa / 男聲 | 1.00 | 1.00 (0.98) | 1.00 (0.99) | 1.00 (0.98) | **0.00** (0.94) |
| hey_jarvis / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.98) | 1.00 (0.98) | 0.66 (0.94) |
| hey_jarvis / 男聲 | 1.00 | 1.00 (0.99) | 1.00 (0.99) | 1.00 (0.99) | **0.00** (0.95) |
| hey_mycroft / 女聲 | 0.00 | 0.00 (0.97) | 0.00 (0.98) | 0.00 (0.98) | 0.00 (0.95) |
| hey_mycroft / 男聲 | 0.99 | **0.01** (0.99) | 0.94 (0.99) | **0.02** (0.99) | **0.00** (0.93) |

- hey_mycroft / 女聲在原始聲音就沒有觸發，這一列不能拿來比較。
- 「Hey Siri」合成語音處理後的 STOI（舊定義）：`deep-wide` 0.98–0.99、`apple-deep` 0.98–0.99、`formant` 0.93–0.94。

**`detect` 模式端到端測試**（Python 版；此模式已在 v0.4.1 移除）：用 hey_jarvis 當替身，喚醒詞放在句子中間。

- 偵測器正確觸發，只處理了那一段（約 1.0 s），其他部分和原音完全相同。
- 處理後的聲音再交給另一個偵測器：舊版 30 dB 的 `apple` 分數仍是 1.00；`formant` 降到 0.00–0.04。

**結論**

- **Apple 式的 3/4/5/6 kHz 窄頻陷波，對這三個開源偵測器幾乎沒效。** 它們主要依賴 3 kHz 以下的共振峰資訊，訓練時也做了大量資料增強。
- 把整段 2.8–6.5 kHz 削掉（舊 `deep-wide`、`apple-wide`）只多擋下一組（hey_mycroft / 男聲），其他照樣觸發。對開源偵測器來說，真正有效的還是削到 3 kHz 以下。
- 這和 WWDC 觀眾「還是被叫醒」的回報一致，也和實機結果一致：`always` + 舊 `deep-wide` 仍會叫醒 Siri。

上表是 v0.4.5 以前的強度定義（`deep-wide` = 2.8–6.5 kHz、`formant` = 1–3 kHz + 窄帶）。STOI 只看約 4.3 kHz 以下的頻段，所以對高頻陷波不太敏感。

### 要削哪一段：v0.4.6 的強度是怎麼定的

實機回報舊 `deep-wide` 擋不住之後，用 openWakeWord 的 hey_jarvis 模型再做一次比較：Piper `libritts_r` 40 位語者念「Hey Jarvis, what time is it?」，其中 36 段原音會觸發；各削掉一段頻帶（70 dB）後看還有幾段觸發。

| 削掉的頻帶 | 仍觸發 | 平均分數 | STOI |
|---|---|---|---|
| 不處理 | 36 / 36 | 1.00 | 1.00 |
| 2.8–6.5 kHz（舊 `deep-wide`） | 31 / 36 | 0.89 | 0.96 |
| 2–3 kHz | 31 / 36 | 0.87 | 0.97 |
| 1–2 kHz | 29 / 36 | 0.79 | 0.95 |
| 0.3–1 kHz | 35 / 36 | 0.97 | 0.87 |
| 2.0–6.5 kHz | 8 / 36 | 0.27 | 0.95 |
| 1–3 kHz | 2 / 36 | 0.19 | 0.95 |
| 1–3 kHz + 3/4/5/6 kHz 窄帶（舊 `formant`） | 0 / 36 | 0.07 | 0.94 |
| 1.5–6.5 kHz | 0 / 36 | 0.02 | 0.94 |
| **1.0–6.5 kHz（新 `deep-wide`）** | **0 / 36** | 0.00 | 0.93 |
| 0.8–8 kHz | 0 / 36 | 0.00 | 0.93 |
| 0.6–8 kHz | 0 / 36 | 0.00 | 0.88 |
| 0.4–8 kHz | 0 / 36 | 0.00 | 0.85 |

- 單獨削 1–2 kHz、2–3 kHz 或高音都沒什麼用；要把 **1–3 kHz 整段**拿掉才有效，再往上削到 6.5 kHz 幾乎不再多損失清晰度。
- 所以新的 `deep-wide` 取 1.0–6.5 kHz（涵蓋舊 `deep-wide` 和舊 `formant`，分數降到 0）；新的 `formant` 取 0.5–8 kHz，當作最後手段。
- 這只是**一個**開源偵測器的結果。Siri 的偵測器不公開，新強度在實機上的效果還沒測過。
- Rust 版的量測（合成測試語音）：新 `deep-wide` 目標頻帶衰減 ≥ 40 dB，總能量 −0.4 dB，頻帶外頻譜不變；新 `formant` 總能量 −3.4 dB。

## 限制

- Apple 的方法沒有公開。實機上只削 3 kHz 以上的設定（30 dB 窄帶、2.8–6.5 kHz 70 dB）都擋不住 HomePod / iPhone；v0.4.6 的新強度在實機上的效果**還沒實測**。
- `heysiri` 的模型只用合成語音訓練，真人聲音的辨識率未知；只認「Hey／嘿 + Siri」，不認單獨的「Siri」。
- `sibilant` 會處理到其他含 s 的字（每次約半秒）。
- `heysiri` 和 `sibilant` 只在 /s/ 之後削；如果 Siri 的偵測器在 /s/ 之前（「Hey」）就已經認出來，這兩個模式擋不到。
- `heysiri` / `sibilant` 的延遲約 72.5 ms（`always` 約 12.5 ms），對話時感覺不出來。
