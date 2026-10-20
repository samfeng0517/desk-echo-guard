# 安裝與通話軟體設定

[← 回 README](../README.md)

## 1. 安裝 VB-Cable（必要）

1. 到 <https://vb-audio.com/Cable/> 下載 **VB-CABLE Driver**。解壓縮後，以系統管理員身分執行 `VBCABLE_Setup_x64.exe`，按 Install Driver，然後重新開機。
2. 裝好後會多出兩個裝置：
   - 播放裝置「**CABLE Input**」：程式把處理好的麥克風聲音送到這裡。
   - 錄音裝置「**CABLE Output**」：Discord / LINE 的麥克風改選這個。
3. 要用 [Hey Siri 防誤觸](siri-guard.md)的話，還要第二條虛擬線：在同一頁下載 **VB-CABLE A+B**（捐款軟體）。裝好後會多出 CABLE-A / CABLE-B。
4. 建議到「控制台 → 聲音」，把 CABLE 和喇叭的「進階」格式都設成 **48000 Hz**。程式內部用 48 kHz；Windows 共用模式會自動轉換，但取樣率一致最省事。

### 讓 VB-Cable 的名稱好懂一點（建議）

VB-Cable 的名稱很容易搞混：「CABLE Input」其實是喇叭，「CABLE Output」才是麥克風。程式開啟後，「裝置」區會顯示有沒有找到 VB-Cable。按右邊的「**整理 VB-Cable 裝置…**」，程式會問你兩件事（可以只做其中一件），再在 Windows 的確認視窗按「是」：

1. **改成好懂的名稱**：

| VB-Cable 原本的名稱 | 改名後 | 用途 |
|---|---|---|
| `CABLE Output`（錄音裝置） | **`回聲消除麥克風`** | Discord / LINE 的**麥克風**選這個 |
| `CABLE-A Input`（播放裝置） | **`Siri防護喇叭`** | 開 Siri 防護時，Discord / LINE 的**輸出裝置**選這個 |
| `CABLE Input`（播放裝置） | `EchoGuard 內部輸出（勿選）` | 程式自己用，通話軟體不要選 |
| `CABLE-A Output`（錄音裝置） | `EchoGuard 內部輸入（勿選）` | 程式自己用，通話軟體不要選 |

- 括號裡的驅動名稱（例如 `(VB-Audio Virtual Cable)`）改不掉，會留著。
- 改完要**重新開啟 Discord / LINE** 才會看到新名稱，然後再選一次麥克風。
- 這等於在「控制台 → 聲音 → 裝置內容」手動改名，只改顯示的名稱，不影響驅動。改名前後程式都找得到裝置，不用改任何設定。
- 想全部改回來（原本名稱＋重新啟用）：勾「顯示進階裝置選項」→「還原 VB-Cable（原本名稱、重新啟用）」。

2. **停用用不到的 VB-Cable 裝置**：例如 `CABLE In 16ch`、`CABLE-B Input`、`CABLE-B Output`。停用後 Discord / LINE 的清單裡就看不到它們，和在「控制台 → 聲音」對裝置按右鍵 →「停用」一樣。
   - 本程式會用到的四個裝置不會被停用。
   - 其他軟體（例如 OBS）有用到 CABLE-B 的話，這一項請按「否」。
   - 還原按鈕會重新啟用所有被停用的 VB-Cable 裝置。

命令列：在系統管理員命令提示字元執行 `python -m echoguard --rename-vbcable rename`、`hide`（停用用不到的）或 `restore`（全部還原）。

下面的說明以原本的名稱為主；改過名的話，`CABLE Output` 就是「回聲消除麥克風」，`CABLE-A Input` 就是「Siri防護喇叭」。

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

- **麥克風一律選 `CABLE Output`**（改過名的話是「回聲消除麥克風」）。
- **通話軟體自己的回音消除關掉**，其他處理照下表設定。
- **通話軟體的噪音抑制可以開**（例如 Discord 的 Krisp）。它處理的是本程式**已經消除回聲之後**的聲音，不會影響回聲消除；又比本程式的 WebRTC 降噪更會去除機械鍵盤聲、滑鼠聲這類短促的敲擊聲。

### Discord

使用者設定 → 語音與視訊：

| 項目 | 設定 |
|---|---|
| 輸入裝置 | `CABLE Output (VB-Audio Virtual Cable)`<br>改過名：`回聲消除麥克風 (VB-Audio Virtual Cable)` |
| 輸出裝置 | 不開 Siri 防護：你的喇叭<br>開 Siri 防護：`CABLE-A Input`（改過名：`Siri防護喇叭`） |
| 回音消除 | 關 |
| 噪音抑制 | **Krisp**（建議，可去除鍵盤聲） |
| 自動增益控制 | 關 |
| 進階語音活動 | 關 |
| 輸入靈敏度 | 建議手動，或改用「按鍵發話」 |

### LINE 電腦版

設定 → 通話：

- 麥克風選 `CABLE Output`（改過名：`回聲消除麥克風`）。
- 喇叭選你的喇叭；開 Siri 防護時選 `CABLE-A Input`（改過名：`Siri防護喇叭`）。

LINE 自己的回音處理通常沒有開關，影響不大。若對方覺得聲音忽大忽小，可以切換程式的「自動增益」比較看看。

### NVIDIA Broadcast：請關掉

- 它的「Room Echo Removal」去的是**殘響**，不是**回聲**，解決不了你的問題。
- 它的降噪處理的是**還沒消除回聲**的麥克風聲音，會讓麥克風和參考訊號之間不再是線性關係，反而害回聲消除收斂變差。
- 需要降噪的話，改開 Discord 的 Krisp（在本程式之後處理，不影響回聲消除），或用本程式的「降噪」選項。

### Windows 麥克風音量

到 Windows 錄音裝置，把實體麥克風音量調到**大聲說話時峰值不超過約 -6 dBFS**。

測試發現，輸入接近滿刻度時 WebRTC AEC3 會完全停止消除，而且很久不會恢復。程式偵測到峰值過高時會提醒（視窗狀態列或命令列輸出）。

### 使用順序

**先開本程式，再開通話。** 程式沒在跑的時候，`CABLE Output` 沒有聲音，對方會聽不到你。
