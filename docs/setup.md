# 安裝與通話軟體設定

[← 回 README](../README.md)

## 1. 安裝 VB-Cable（必要）

1. 到 <https://vb-audio.com/Cable/> 下載 **VB-CABLE Driver**。解壓縮後，以系統管理員身分執行 `VBCABLE_Setup_x64.exe`，按 Install Driver，然後重新開機。
2. 裝好後會多出兩個裝置：
   - 播放裝置「**CABLE Input**」：程式把處理好的麥克風聲音送到這裡。
   - 錄音裝置「**CABLE Output**」：Discord / LINE 的麥克風改選這個。
3. 要用 [Hey Siri 防誤觸](siri-guard.md)的話，還要第二條虛擬線：在同一頁下載 **VB-CABLE A+B**（捐款軟體）。裝好後會多出 CABLE-A / CABLE-B。
4. 建議到「控制台 → 聲音」，把 CABLE 和喇叭的「進階」格式都設成 **48000 Hz**。程式內部用 48 kHz；Windows 共用模式會自動轉換，但取樣率一致最省事。

## 2. 安裝程式

### 方法 A：下載 exe（推薦，不用裝 Python）

1. 到 [Releases](https://github.com/samfeng0517/desk-echo-guard/releases) 下載最新的 `DeskEchoGuard.exe`。
2. 放在任何資料夾，雙擊就會開啟設定視窗，用法見[使用說明](usage.md)。

注意：

- exe 約 90 MB，第一次開啟要先解壓縮，會等 5–15 秒才出現視窗。
- exe 沒有程式碼簽章，Windows 可能顯示「Windows 已保護您的電腦」。按「其他資訊 → 仍要執行」即可。
- 想確認 exe 本身沒問題，在命令提示字元執行 `DeskEchoGuard.exe --self-test report.txt`，`report.txt` 最後一行應是 `RESULT: PASS`。報告也會列出它找到的音效裝置。

### 方法 B：從原始碼執行

建議用 Python 3.11 或 3.12（64 位元）：

```bat
cd desk-echo-guard
py -3.12 -m venv .venv
.venv\Scripts\activate
pip install -r requirements.txt
:: 要用 Hey Siri 的 detect 模式（喚醒詞模型）再加：
pip install -r requirements-siri.txt
```

| 套件 | 用途 |
|---|---|
| `soundcard` | Windows 錄放音、擷取喇叭正在播的聲音（loopback） |
| `livekit` | WebRTC 回聲消除（AEC3）、降噪、高通，內含 Windows 原生 DLL |
| `pyaec` | SpeexDSP 回聲消除（備援引擎） |

確認引擎都能載入：

```bat
python -m echoguard --list-engines
:: 預期：available AEC engines: webrtc, speex, nlms, off
```

雙擊專案資料夾裡的 `start_gui.bat` 就會開設定視窗。它會優先用上面建立的 `.venv`，沒有的話改用 Python 啟動器 `pyw`。

## 3. 通話軟體與音效設定

整條路怎麼接，見 [運作原理](how-it-works.md#接線)。重點是兩件事：

- **麥克風一律選 `CABLE Output`**。
- **通話軟體自己的回音、噪音處理全部關掉**。它們會和本程式打架，讓回聲消除收斂變差。

### Discord

使用者設定 → 語音與視訊：

| 項目 | 設定 |
|---|---|
| 輸入裝置 | `CABLE Output (VB-Audio Virtual Cable)` |
| 輸出裝置 | 不開 Siri 防護：你的喇叭<br>開 Siri 防護：`CABLE-A Input` |
| 回音消除 | 關 |
| 噪音抑制 | 無（不要選 Krisp） |
| 自動增益控制 | 關 |
| 進階語音活動 | 關 |
| 輸入靈敏度 | 建議手動，或改用「按鍵發話」 |

### LINE 電腦版

設定 → 通話：

- 麥克風選 `CABLE Output`。
- 喇叭選你的喇叭；開 Siri 防護時選 `CABLE-A Input`。

LINE 自己的回音處理通常沒有開關，影響不大。若對方覺得聲音忽大忽小，可以切換程式的「自動增益」比較看看。

### NVIDIA Broadcast：請關掉

- 它的「Room Echo Removal」去的是**殘響**，不是**回聲**，解決不了你的問題。
- 它的降噪會讓麥克風和參考訊號之間不再是線性關係，反而害回聲消除收斂變差。
- 需要降噪的話，改用本程式的「降噪」選項（WebRTC 降噪）。

### Windows 麥克風音量

到 Windows 錄音裝置，把實體麥克風音量調到**大聲說話時峰值不超過約 -6 dBFS**。

測試發現，輸入接近滿刻度時 WebRTC AEC3 會完全停止消除，而且很久不會恢復。程式偵測到峰值過高時會提醒（視窗狀態列或命令列輸出）。

### 使用順序

**先開本程式，再開通話。** 程式沒在跑的時候，`CABLE Output` 沒有聲音，對方會聽不到你。
