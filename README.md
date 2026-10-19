# desk-echo-guard

這是 Windows 用的全系統回聲消除程式，給用桌上型喇叭和桌上型麥克風講 Discord / LINE 通話的人。另外附一個選用的「Hey Siri 防誤觸」濾波器。

- **管線 A：麥克風回聲消除（AEC）**
  - 用 WASAPI loopback 擷取喇叭正在播的聲音，當作回聲參考訊號，再用 WebRTC AEC3 把它從麥克風裡扣掉。
  - 處理後的聲音送到 VB-Cable，Discord / LINE 的麥克風改選 **CABLE Output**。
  - 解決的問題：對方從你的麥克風聽到自己的回音。
- **管線 B：Hey Siri 防誤觸（`--siri-guard`）**
  - 通話聲音先送到第二條虛擬線（VB-Cable A），程式緩衝約 1 秒、偵測喚醒詞，在「Siri」那一段做窄頻陷波，再播到真正的喇叭。
  - 這是仿照 Apple 在 WWDC 2026 發表會被觀察到的做法，不是靜音。
  - 解決的問題：對方說「Hey Siri」，結果你的 HomePod / iPhone 被叫醒。

> **誠實聲明**
> - 這個程式是在沒有音效裝置的雲端 Linux 環境開發的。DSP 核心都用合成訊號、TTS 語音做過離線測試，也跑過模擬多裝置時脈的執行緒整合測試。
> - **但沒有在真的 Windows 電腦、真的喇叭和麥克風上跑過。** 需要你實測的項目列在[最後一節](#需要在-windows-實機驗證的項目)。
> - Apple 的防誤觸方法**沒有官方公開**。下面的實作是根據第三方頻譜分析做的近似版本，效果請看[研究摘要](#hey-siri-防誤觸apple-怎麼做研究摘要)。

---

## 1. 原理與接線

```
                       ┌──────────── 管線 A ────────────┐
 實體麥克風 ──────────► │ WebRTC AEC3 (+降噪/高通)      │ ──► CABLE Input ══► CABLE Output ──► Discord/LINE 麥克風
                       │        ▲ 回聲參考               │
 喇叭 loopback ────────► │────────┘ (彈性緩衝+時脈漂移補償) │
                       └───────────────────────────────┘

 (選用 --siri-guard)
 Discord/LINE 輸出 ──► CABLE-A Input ══► CABLE-A Output ──► 延遲 1 s + 偵測 + 陷波 ──► 真正的喇叭
```

- 回聲參考取自**真正喇叭的 loopback**，也就是經過管線 B 之後、最後真的從喇叭播出來的聲音，連其他程式（YouTube、遊戲）的聲音都會一併消掉。這正是麥克風收到的回聲來源。
- 每個音效裝置都有自己的時脈，彼此會差幾十到幾百 ppm。程式在裝置之間用「彈性緩衝」連接：以時間戳記內插估計水位，再用臨界阻尼 PI 控制器微調重取樣比例，補償時脈漂移（測試中 ±500 ppm 都追得到，誤差小於 1 ppm）。

## 2. 安裝

### 2.1 VB-Cable

1. 到 <https://vb-audio.com/Cable/> 下載 **VB-CABLE Driver**。解壓縮後，以系統管理員身分執行 `VBCABLE_Setup_x64.exe`，按 Install Driver，然後重新開機。
2. 裝完會多出：
   - 播放裝置「**CABLE Input**」
   - 錄音裝置「**CABLE Output**」
3. 要用 `--siri-guard` 的話，還要第二條線：在同一頁下載 **VB-CABLE A+B**（捐款軟體）。安裝後會多出 CABLE-A / CABLE-B。
4. 建議到「控制台 → 聲音」，把 CABLE 和喇叭的「進階」格式都設成 **48000 Hz**。程式內部是 48 kHz，WASAPI 共用模式也會自動轉換，但同一個取樣率最省事。

### 2.2 最簡單：直接下載 exe（不用裝 Python）

1. 到 [Releases](https://github.com/samfeng0517/desk-echo-guard/releases) 下載最新的 `DeskEchoGuard.exe`。
2. 放在任何資料夾，雙擊就會打開設定視窗（用法見 3.0 節）。

注意：

- 單一 exe 約 100 MB 以上；第一次開啟要先解壓縮到暫存資料夾，會等 5–15 秒。
- 這個 exe 沒有程式碼簽章，Windows SmartScreen 可能顯示「Windows 已保護您的電腦」。按「其他資訊 → 仍要執行」即可。
- 想確認 exe 本身沒問題：在命令提示字元執行 `DeskEchoGuard.exe --self-test report.txt`，再打開 `report.txt`，最後一行應是 `RESULT: PASS`。
- exe 由 GitHub Actions 在 Windows 上自動打包：每次推到 `main` 都會產生（在 Actions 頁面的 Artifacts）；打 `v*` 標籤，或在 Actions 頁面手動執行「Build Windows exe」並填 `release_tag`，會發佈到 Releases。也可以自己執行 `packaging\build_exe.bat` 打包。

用 exe 的話可以跳過 2.3。

### 2.3 Python 套件（從原始碼執行）

建議用 Python 3.11 或 3.12（64 位元）：

```bat
cd desk-echo-guard
py -3.12 -m venv .venv
.venv\Scripts\activate
pip install -r requirements.txt
:: 要用 Hey Siri 偵測模式（detect）再加：
pip install -r requirements-siri.txt
```

`requirements.txt` 裝的主要套件：

| 套件 | 用途 |
|---|---|
| `soundcard` | WASAPI 錄放音、喇叭 loopback |
| `livekit` | WebRTC AudioProcessing（AEC3、降噪、高通），內含 Windows 原生 DLL |
| `pyaec` | SpeexDSP 回聲消除，備援引擎 |

確認引擎都抓得到：

```bat
python -m echoguard --list-engines
:: 預期輸出：available AEC engines: webrtc, speex, nlms, off
```

## 3. 使用

### 3.0 圖形介面（不想用命令列的話）

**雙擊 `DeskEchoGuard.exe`**（2.2 節），或在專案資料夾裡雙擊 `start_gui.bat`，會開一個設定視窗（不會跳出黑色命令列視窗）。也可以手動執行 `pythonw -m echoguard.gui`。（`start_gui.bat` 會優先用 2.3 節建立的 `.venv`，沒有的話改用 Python 啟動器 `pyw`。）

- 視窗裡從下拉選單挑麥克風、喇叭和 CABLE Input，選回聲消除引擎，再勾要不要開 Hey Siri 防誤觸，然後按「▶ 開始」。
- 狀態區會即時顯示麥克風／喇叭／輸出音量條、緩衝和時脈漂移；麥克風太大聲時會提醒。
- 「量測延遲」會播 4 秒雜訊，並告訴你要不要調延遲。
- 設定在按「開始」或關閉視窗時自動存到 `%APPDATA%\desk-echo-guard\echoguard.toml`，下次開啟會沿用。這個檔案也可以給命令列的 `--config` 用。
- 執行中所有設定欄位都會鎖住；要改設定請先按「■ 停止」，改完再按「▶ 開始」。

以下 3.1–3.4 是命令列用法，用 GUI 的話可以跳過。

### 3.1 列出裝置

```bat
python -m echoguard --list-devices
```

裝置名稱都用「**部分字串、不分大小寫**」比對，例如 `--mic "USB"`、`--speaker "Realtek"`。

### 3.2 只開回聲消除

```bat
python -m echoguard --mic "你的麥克風" --speaker "你的喇叭" --out "CABLE Input"
```

每 5 秒會印一行狀態：麥克風／參考／輸出音量、緩衝水位、漂移 ppm、欠載次數、每 10 ms 的運算時間。

### 3.3 回聲消除 + Hey Siri 防誤觸

```bat
python -m echoguard --mic "USB" --speaker "Realtek" --siri-guard --siri-mode always
```

### 3.4 用設定檔

```bat
copy echoguard.example.toml echoguard.toml
python -m echoguard --config echoguard.toml
```

鍵名就是命令列長選項名稱，`-` 或 `_` 都可以。命令列參數會覆蓋設定檔裡的值。

### 3.5 Discord 設定（重要）

1. 使用者設定 → 語音與視訊：
   - **輸入裝置**：`CABLE Output (VB-Audio Virtual Cable)`
   - **輸出裝置**：
     - 不開 `--siri-guard`：選你的喇叭。
     - 開 `--siri-guard`：選 `CABLE-A Input`。
2. **關掉** Discord 自己的處理，避免和這個程式打架：
   - 「回音消除」：關
   - 「噪音抑制」：選「無」（不要選 Krisp）
   - 「自動增益控制」：關
   - 「進階語音活動」：關
3. 輸入靈敏度建議改成手動，或改用「按鍵發話」。

### 3.6 LINE 電腦版設定

設定 → 通話：

- 麥克風選 `CABLE Output`。
- 喇叭依是否開 `--siri-guard`，選喇叭或 `CABLE-A Input`。

LINE 自己的回音處理通常沒有開關，影響不大，但若對方覺得聲音忽大忽小，可以試 `--agc on` 或 `--agc off` 比較。

### 3.7 NVIDIA Broadcast

**請關掉，或不要把它串在這條路上。** 原因：

- 它的「Room Echo Removal」是去**殘響**，不是去**回聲**。
- 它的降噪會讓麥克風和參考訊號之間不再是線性關係，反而害 AEC 收斂不好。
- 要用它的降噪的話，請改用本程式的 `--noise-suppression on`（WebRTC 降噪）。

### 3.8 麥克風音量

在 Windows 錄音裝置把實體麥克風的音量調到**大聲說話時峰值不超過約 -6 dBFS**。

測試發現：輸入接近滿刻度時，AEC3 會完全停止消除。程式偵測到峰值超過 0.9 時會印出警告。

---

## 4. 調校

| 情況 | 處理 |
|---|---|
| 開頭幾秒還有回聲 | 正常，AEC 需要約 1–3 秒收斂（WebRTC），Speex 約 5–10 秒。 |
| 對方說雙方同時講話時，你的聲音被吃掉／斷斷續續 | 測試量到 WebRTC AEC3 在強回聲的雙講時會壓低近端語音（見下表）。改試 `--engine speex`：雙講保留度明顯較好，但頻寬只到約 7 kHz，收斂較慢。 |
| 一直有殘留回聲 | 先執行 `python -m echoguard --calibrate --mic ... --speaker ...`：播 4 秒雜訊，量測參考到麥克風的延遲。<br>- WebRTC 會自己估延遲，通常不用設定。<br>- speex / nlms 若延遲超過濾波器長度（預設 300 ms），用 `--delay-ms` 補。<br>- 若顯示參考訊號「晚到」（負延遲），用 `--mic-delay-ms` 把麥克風延後。 |
| 參考緩衝一直欠載（`ur=` 一直增加） | 加大 `--ref-buffer-ms`，例如 30–40。 |
| 想錄下來分析 | 加 `--debug-wav debug`，結束時會存 `mic.wav`、`ref.wav`、`out.wav`、`call.wav`（最多 10 分鐘）。<br>之後可以離線重跑：`python -m echoguard --sim-mic debug\mic.wav --sim-ref debug\ref.wav --sim-out o.wav --engine speex` |
| 喇叭開很大聲、喇叭失真 | 線性 AEC 消不掉失真的部分。調小喇叭或調低系統音量，或把喇叭移離麥克風。 |

**延遲預算（模擬量測）**

- 麥克風路徑約 60–70 ms：輸出緩衝 40 ms、10 ms 音框、AEC3 約 9 ms。
- Siri 防護：
  - `detect` / `sibilant` 模式，通話聲音會多 **約 1.0 s** 延遲。
  - `always` 模式只多約 12.5 ms（FIR 群延遲）。

---

## 5. Hey Siri 防誤觸：Apple 怎麼做（研究摘要）

### 5.1 找到的資料

1. **WWDC 2026 主題演講的頻譜分析**
   - 荷蘭音訊工程師 Luuk de Leest 在 X 上貼出頻譜圖：主題演講裡每次講到「Siri」時，**約 3 kHz、4 kHz、5 kHz、6 kHz 四個窄頻帶被削掉**，其他時間的音軌沒有動。
   - 報導把它描述成「**動態陷波濾波器（dynamic notch filter）**」：只在講到觸發詞的瞬間啟動。
   - 觀眾仍聽得懂「Siri」，只是稍微變薄，或有點「金屬感」。
   - **Apple 沒有官方證實。**
   - **效果不完全**：多名觀眾回報 iPhone / HomePod 還是被叫醒，報導歸因於麥克風特性、房間聲學和音量。
   - 來源：
     - [MacRumors](https://www.macrumors.com/2026/06/12/apple-cut-frequencies-to-prevent-siri-activations/)
     - [heise online](https://www.heise.de/en/news/WWDC-2026-Apple-filtered-frequencies-to-prevent-Siri-activations-11333194.html)
     - [TechRadar](https://www.techradar.com/ai-platforms-assistants/an-apple-siri-mystery-has-been-solved-by-an-audio-engineer-spectrogram-reveals-how-apple-stopped-voice-assistant-setting-off-millions-of-iphones-and-homepods-during-its-wwdc-2026-event)
     - [Wccftech](https://wccftech.com/apple-modified-the-frequencies-of-its-wwdc-2026-keynote-to-avoid-triggering-your-devices-each-time-siri-was-mentioned/)
     - [Stuff](https://www.stuff.tv/news/well-isnt-this-neat-apple-modified-the-way-siri-was-spoken-during-its-keynote-to-stop-your-iphone-from-listening/)
     - 原始貼文：[@luuk58](https://x.com/luuk58/status/2064085109980987720)、[tech.explain](https://x.com/techexplain1/status/2069494958768504836)
   - **公開資料沒有**：陷波的寬度、深度（dB）、起落時間。
2. **Apple 的 Hey Siri 偵測器本身**（[Apple Machine Learning Research, 2017](https://machinelearning.apple.com/research/hey-siri)）
   - 16 kHz 取樣，每 10 ms 一個 mel 頻譜框，約 0.2 秒的視窗送進 DNN。
   - 兩段式：先在低功耗 Always-On Processor 跑小模型，再由主處理器的大模型確認。
   - 有「說話者辨識」：用註冊時錄的 5 句比對，可降低別人聲音觸發的機率。所以遠端的人說「Hey Siri」，**本來就不一定**叫得醒你的裝置。
   - 文章**沒有**說明任何針對電視或媒體聲音的防護機制。
   - 相關專利：[US20140222436A1 Voice trigger for a digital assistant](https://patents.google.com/patent/US20140222436A1/en)。也沒有描述廣播聲音的抑制。
3. **其他公司的前例**
   - Amazon 2017 年的 Alexa 廣告：Reddit 網友發現 3–6 kHz 被大幅衰減，Amazon 從未證實。
   - Amazon 官方說法是**聲紋指紋（acoustic fingerprinting）**：事先把廣告音訊做指紋，放到 Echo 上比對後忽略；也會即時偵測「很多家庭同時聽到同一句指令」（[Amazon Science](https://amazon.science/blog/why-alexa-wont-wake-up-when-she-hears-her-name-in-amazons-super-bowl-ad)、[Popular Science](https://www.popsci.com/why-amazon-alexa-can-ignore-television/)）。
   - Google 有「錄製媒體熱詞抑制」專利（[US10867600B2](https://patents.google.com/patent/US10867600)），同樣靠指紋比對。
   - 這兩種方法都要**裝置端配合**，我們沒辦法對你的 HomePod 使用。所以在訊號端能做的，只剩頻譜修改這條路。

### 5.2 本專案的實作（`echoguard/siri_guard.py`）

- **陷波濾波器**：線性相位 FIR 多頻帶阻帶（1201 taps，群延遲 12.5 ms）。
  - 線性相位的好處：處理過的訊號和延遲後的原音逐樣本對齊，可以用 `out = dry − m(t)·(dry − wet)` 平滑淡入淡出（預設 15 ms），不會產生梳狀濾波。
- **時間控制**：通話聲音延遲 `lookahead`（預設 1 s），偵測器跑在**未延遲**的輸入上。偵測器要等整句講完才會觸發，那時那一句還在緩衝區裡，所以能對整句做陷波後再播出。
- **模式（`--siri-mode`）**：
  - `detect`：openWakeWord 偵測喚醒詞，命中就對前約 0.9 s 做陷波。最接近 Apple 的「只在講 Siri 時動手」。需要自行訓練 hey siri 模型，見 [tools/train_hey_siri](tools/train_hey_siri/README.md)。
  - `sibilant`：不需模型。偵測 /s/ 類高頻爆發（Siri 的開頭），就陷波 0.45 s。缺點是其他含 s 的字也會被處理到。
  - `always`：全程套用陷波，不偵測、不加 1 秒延遲。
- **頻帶設定（`--siri-profile`）**：

  | profile | 阻帶 | 深度 | 說明 |
  |---|---|---|---|
  | `apple`（預設） | 3/4/5/6 kHz，各寬 400 Hz | 30 dB | 照 WWDC 2026 觀察複製；深度是猜的 |
  | `apple-deep` | 同上 | 70 dB | |
  | `apple-wide` | 2.8–6.5 kHz | 50 dB | |
  | `formant` | 1–3 kHz（第二共振峰區）+ 上述四個窄帶 | 70 dB | 實驗證實對開源偵測器有效，但聲音較明顯變悶 |

### 5.3 實測效果

**拿不到 Siri 的偵測器**，所以用 openWakeWord 的開源模型（alexa、hey_jarvis、hey_mycroft）當替身，對 Piper TTS 合成的兩種聲音做測試。

- 表格數字 = 偵測分數（0–1，≥0.5 算觸發）；括號 = STOI 清晰度。
- 重現方式：`experiments/guard_proxy_eval.py`

| 片語/聲音 | 原始 | apple | apple-deep | apple-wide | formant |
|---|---|---|---|---|---|
| alexa / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.98) | 1.00 (0.98) | **0.01** (0.94) |
| alexa / 男聲 | 1.00 | 1.00 (0.99) | 1.00 (0.99) | 1.00 (0.98) | **0.00** (0.93) |
| hey_jarvis / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.98) | 1.00 (0.98) | **0.30** (0.93) |
| hey_jarvis / 男聲 | 1.00 | 1.00 (0.99) | 1.00 (0.99) | 0.99 (0.99) | **0.01** (0.93) |
| hey_mycroft / 女聲 | 1.00 | 1.00 (0.98) | 1.00 (0.98) | 1.00 (0.99) | **0.00** (0.92) |
| hey_mycroft / 男聲 | 1.00 | 1.00 (0.99) | 1.00 (0.99) | 1.00 (0.99) | **0.00** (0.94) |

**端到端測試**：用 hey_jarvis 當替身跑 `detect` 模式，喚醒詞在句子中間。

- 偵測器正確觸發，只處理了那一段（約 1.0 s）。
- 處理後再交給另一個偵測器：
  - `apple`：分數仍是 1.00
  - `formant`：降到 0.00–0.04

**結論（請務必看）**

- **Apple 式的 3/4/5/6 kHz 窄頻陷波，對這三個開源偵測器完全沒效。** 它們訓練時做了大量資料增強，主要靠 3 kHz 以下的共振峰資訊。
- 這和 WWDC 觀眾「仍然被叫醒」的回報一致。
- Siri 的偵測器是否比較吃 3–6 kHz 的 /s/ 音，**目前沒有公開證據**，只有 Apple 自己選了這些頻帶這個間接線索。
- 建議用法：
  1. 先用預設 `apple`，看你的 HomePod / iPhone 還會不會被叫醒。
  2. 還會的話，改 `--siri-profile formant`。搭配 `detect` 模式，只有約 1 秒的喚醒詞會變悶，其餘通話不受影響。
- 「Hey Siri」TTS 的 STOI：`apple` 約 0.98–0.99，`formant` 約 0.94。注意 STOI 只看約 4.3 kHz 以下，所以對高頻陷波不敏感；log-spectral distance 另見實驗輸出。

---

## 6. 離線驗證結果（雲端環境，無音效裝置）

執行方式：

```bash
pip install -r requirements-dev.txt
pytest -s          # 34 passed, 2 skipped（nlms 沒有降噪，所以跳過那兩組；GUI 測試需要 tkinter 和螢幕，Linux 可用 xvfb-run）
```

### 6.1 AEC：合成訊號

- 合成訊號：類語音的脈衝列加共振峰、摩擦音、停頓。
- 麥克風 = 近端語音 + 房間脈衝響應（直達聲 + 200 ms RT60 尾音）× 延遲後的參考 + 雜訊。
- 測試順序：只有遠端 40% → 只有近端 20% → 雙講 40%。

| 引擎 | NS | 延遲 | 遠端單講 ERLE | 近端單講 音量變化 / STOI | 雙講 STOI 輸出（原始麥克風） |
|---|---|---|---|---|---|
| webrtc | 關 | 40 ms | **42.0 dB** | −0.4 dB / 0.99 | 0.64 (0.70) |
| webrtc | 開 | 40 ms | **54.1 dB** | −0.4 dB / 0.99 | 0.62 (0.70) |
| webrtc | 關 | 150 ms | **42.2 dB** | −0.4 dB / 0.99 | 0.61 (0.68) |
| speex | 關 | 40 ms | 25.9 dB | −2.1 dB / 0.99 | 0.92 (0.70) |
| speex | 開 | 40 ms | 39.1 dB | −2.2 dB / 0.93 | 0.90 (0.70) |
| speex | 關 | 150 ms | 22.4 dB | −2.1 dB / 0.99 | 0.92 (0.68) |
| nlms | — | 40 ms | 25.9 dB | −0.1 dB / 1.00 | 0.87 (0.70) |
| nlms | — | 150 ms | 14.7 dB | −0.1 dB / 1.00 | 0.89 (0.68) |

### 6.2 AEC：TTS 真人語音

遠端女聲、近端男聲（`experiments/aec_tts_eval.py`）：

| 引擎 | NS | 延遲 | ERLE | 近端單講 STOI | 雙講 STOI 輸出（麥克風） |
|---|---|---|---|---|---|
| webrtc | 關 | 40 ms | **41.9 dB** | 1.00 | 0.59 (0.74) |
| webrtc | 開 | 40 ms | **53.9 dB** | 0.99 | 0.55 (0.74) |
| speex | 關 | 40 ms | 23.5 dB | 0.99 | **0.98** (0.74) |
| speex | 開 | 40 ms | 43.3 dB | 0.97 | **0.94** (0.74) |
| nlms | — | 40 ms | 18.0 dB | 1.00 | 0.92 (0.74) |
| webrtc | 關 | 150 ms | 42.2 dB | 0.99 | 0.60 (0.74) |
| speex | 開 | 150 ms | 38.1 dB | 0.95 | 0.94 (0.74) |

雙講的解讀：

- WebRTC AEC3（透過 livekit，無法調參）回聲壓得最乾淨，但雙講時會壓低近端語音。
- 回聲越小，壓低越少。近端/回聲比 +14 dB 時，STOI 0.88，原始麥克風是 0.96。
- Speex 在雙講保留得好很多。

### 6.3 其他已驗證項目

- **時脈漂移**：彈性緩衝對 ±500 ppm 的生產端，估計值 499.5 / −499.8 ppm，水位維持在 10–20 ms，沒有欠載。
- **欠載時保持時間對齊**：讀取位置繼續前進，晚到的樣本直接跳過，所以參考和麥克風的對齊不會被打亂。
- **整合測試**：完整多執行緒程式接模擬裝置，各裝置時脈不同，loopback +100 ppm、CABLE −60 ppm，2 倍速執行。
  - ERLE 約 40–41.6 dB，沒有欠載。
  - Siri 防護路徑的 4 kHz 頻帶衰減 27 dB。
- **延遲估計（GCC-PHAT）**：誤差 < 1 ms。負延遲（參考晚到）也能正確判讀。
- **延遲補償**：600 ms 大延遲時，nlms ERLE 從 −2.8 dB 變成 17.5 dB（加 `--delay-ms 590`）。
- **Siri 陷波**（`always`、合成語音）：

  | profile | 目標頻帶衰減 | 總能量變化 | 頻帶外 LSD | STOI |
  |---|---|---|---|---|
  | `apple` | ≥ 26.7 dB | −0.13 dB | 0.24 dB | 0.99 |
  | `formant` | ≥ 32 dB | — | — | 0.89 |

- **`detect` 模式**（理想偵測器）：只有片語區段被改，其餘樣本與原音差異 0。區段內目標頻帶衰減 26.5 dB，總延遲 1012.5 ms。
- **測試時發現的 AEC3 行為**：
  - 輸入接近滿刻度，或出現極不自然的大聲嘶聲時，AEC3 會進入「不消除」狀態，之後很久都不會恢復。
  - 真實語音（TTS）沒有遇到，但這是程式會警告麥克風峰值的原因。

---

## 7. 限制

- **只在模擬環境驗證過**，見下一節。
- 線性 AEC 消不掉喇叭失真或機殼共振。喇叭開太大聲時，殘留回聲會變多。
- WebRTC 引擎透過 `livekit` 的 FFI 呼叫，AEC3 參數不能調（例如雙講的積極程度）。
- Speex 和 NLMS 只在 16 kHz 處理，麥克風頻寬會變成約 7 kHz。
- 回聲參考是喇叭的 loopback。Windows 擷取的是混音後、硬體音量前的訊號，所以「系統主音量」大幅改變時，AEC 需要重新收斂幾秒。這是推論，未實測。
- Siri 防護：
  - Apple 的方法沒有公開，效果無法在雲端驗證。
  - `detect` 模式需要自行訓練模型，沒有實測。
  - `sibilant` 模式會影響其他含 s 的字。
  - `detect` / `sibilant` 模式讓通話聲音延遲約 1 秒。對話時會感覺到，`always` 模式沒有這個延遲。
- 程式關掉後，Discord / LINE 的麥克風（CABLE Output）會變成沒有聲音。記得先開程式再開通話。

## 8. 需要在 Windows 實機驗證的項目

1. `soundcard` 在你的電腦上：
   - `--list-devices` 正常列出裝置
   - 喇叭 loopback 能錄到聲音
   - 寫入 CABLE Input 不爆音、不欠載
   - 如果狀態列的 `ur=` 一直增加，調整 `--blocksize` 或緩衝大小。
2. `livekit` 的 WebRTC 引擎在 Windows 能載入（`--list-engines` 有 `webrtc`），每 10 ms 運算時間遠小於 10 ms。Linux 上量到約 0.4–0.6 ms。
3. `--calibrate` 量到的延遲是正值且信心度 > 5。
4. 實際通話時的 ERLE。用 `--debug-wav` 錄下 mic/ref/out 後可離線比較，也可以請對方回報有沒有回音。
5. 雙講時你的聲音會不會被吃掉。會的話試 `--engine speex`。
6. `--siri-guard`：
   - Discord 輸出改到 CABLE-A Input 後，喇叭聽得到對方
   - 延遲可以接受
   - 用另一台裝置播「Hey Siri」影片，比較 `apple` / `formant` profile 下 HomePod / iPhone 是否被叫醒
7. 長時間（1 小時以上）執行時，漂移 ppm 穩定、緩衝水位沒有持續增減。

## 9. 專案結構

```
echoguard/
  aec.py          AEC 引擎：webrtc(livekit AEC3) / speex(pyaec) / nlms(純 numpy) + 延遲線
  buffers.py      ElasticBuffer（時脈漂移補償）、DelayLine
  resample.py     串流整數倍重取樣（48k↔16k）
  delay.py        GCC-PHAT 延遲估計
  siri_guard.py   線性相位陷波、偵測器（openWakeWord / sibilant / oracle）、SiriGuard
  app.py          多執行緒即時管線、--calibrate
  io_backend.py   裝置 I/O 介面 + soundcard（WASAPI）實作
  sim.py          模擬裝置後端（測試用，可設各裝置 ppm）
  signals.py / metrics.py   合成測試訊號、ERLE / STOI / 頻譜距離等量測
  cli.py          命令列
  gui.py          tkinter 設定視窗（start_gui.bat 啟動）
  selftest.py     免音效裝置的自我檢測（exe 的 --self-test）
packaging/        PyInstaller 進入點與 build_exe.bat
.github/workflows/build-exe.yml   在 Windows 上跑測試、打包 exe、發佈 Release
tests/            pytest（離線 DSP、執行緒整合、CLI）
experiments/      TTS + openWakeWord 的評估腳本（需網路）
tools/train_hey_siri/   自訂 hey siri 模型的訓練設定與說明
```
