# GUI spec (Windows 11 / Fluent)

Audience: someone who has never heard of echo cancellation or VB-Cable and
is setting this up for a Discord / LINE call — e.g. a friend of the author
who today needs it explained in person.  Every screen must answer "what is
this, do I need to touch it, what do I do next" on its own.

All user-facing text is Traditional Chinese (Taiwan usage), short, plain,
no jargon without an explanation ("回聲參考", "AEC", "ppm", "loopback"
never appear without a plain-words gloss; prefer not to show them at all on
the main pages).

## Look and feel

* Slint `fluent` style (Windows 11 controls), follows the system light /
  dark theme, accent colour from Windows when available.
* Mica window backdrop on Windows 11 (fallback: solid `#f3f3f3` / `#202020`).
* Font: Segoe UI Variable / Microsoft JhengHei UI; body 14 px, captions
  12 px, page titles 28 px semibold, section titles 20 px semibold.
* 4 px grid, 8 px corner radius cards, "settings card" rows exactly like
  Windows 11 Settings: leading icon, title, one-line description underneath
  in secondary text colour, control on the right; expandable cards for
  rarely used groups.
* DPI aware (per-monitor v2), min window size 760 × 560, default 900 × 640.
* Status uses Fluent InfoBars (informational / success / warning / error
  colours with an icon, a title, a message and at most one action button).

## Navigation

Left navigation pane (icons + labels, collapsible), five items:

1. **首頁** – status and the one big button
2. **裝置** – microphone, speakers, VB-Cable
3. **Hey Siri 防誤觸**
4. **進階** – sound tuning, delay measurement, start-up, logs
5. **說明** – step-by-step guide, FAQ, re-run the setup wizard

## Interaction rules (fewer buttons)

* No "儲存設定" button: every change is saved immediately.
* No "重新整理裝置" button: the device lists refresh by themselves
  (Windows device-change notifications, plus a refresh whenever a page with
  device lists is shown).
* Settings are **not** locked while running.  A change is applied by
  restarting the audio pipeline automatically (debounced ~0.8 s); a small
  "已套用變更" toast confirms it.  (The old GUI locked every field and made
  the user stop / start by hand.)
* The start / stop control is a single accent button on the home page (and
  in the tray menu).
* Destructive / admin actions (VB-Cable rename, disable, restore) open a
  confirmation dialog that explains exactly what will change and that
  Windows will ask for permission.
* Closing the window hides it to the tray; the first time, a notification
  says so and how to quit.

## First-run setup wizard

Shown when no settings file exists (and from 說明 → "重新執行設定精靈").
Full window, step indicator at the top ("步驟 2 / 5"), 上一步 / 下一步 at the
bottom right, "略過精靈" link at the bottom left.

1. **歡迎** – what the program does in three bullets, with a simple
   diagram: 喇叭 → 麥克風收到回音 → 本程式把回音扣掉 → Discord / LINE.
   "大約需要 5 分鐘。"
2. **安裝 VB-Cable** – detects it.  Missing: explain what it is ("一條
   虛擬的音源線，本程式用它把處理好的聲音交給 Discord / LINE"), button
   "下載 VB-Cable", numbered install steps (解壓縮 → 以系統管理員身分執行
   VBCABLE_Setup_x64.exe → Install Driver → 重新開機), re-checks by itself
   every few seconds; found: success InfoBar.  Offer "把 VB-Cable 改成好懂
   的名稱（建議）" as a checkbox, done at the end of the wizard.
3. **麥克風與喇叭** – two combo boxes (default entry shows the current
   Windows default: "Windows 預設（Realtek 麥克風）").  Live meters with
   instructions: "對著麥克風說話，上面那條應該會跳動" and a "播放測試音"
   button that plays 1 s of a soft tone on the chosen speakers so the
   喇叭 meter moves.  Warning InfoBar if the mic clips.
4. **設定通話軟體** – tabs Discord / LINE / 其他軟體; numbered steps with the
   exact menu path and the exact device name to pick, each name with a copy
   button; "關掉通話軟體自己的回音消除" explained; Discord: Krisp may stay
   on; NVIDIA Broadcast off.
5. **完成** – toggle "登入 Windows 時自動啟動（建議）", button
   "開始使用" (starts the pipeline and opens 首頁).

## 首頁

* Hero card: large shield icon, state title "回聲消除：運作中" / "已停止",
  one sentence ("對方聽不到自己的回音了。" / "按「開始」後，對方就不會
  聽到回音。"), accent button **開始** / **停止**.
* "在通話軟體這樣設定" card: "麥克風（輸入裝置）選：**回聲消除麥克風**"
  [複製]; when the Siri guard is on also "喇叭（輸出裝置）選：**Siri防護喇叭**".
  Link "看圖解步驟" → 說明.
* Live levels card, three meters with plain names and one-line hints:
  - 你的麥克風 – "你說話時會跳動"
  - 喇叭播放中 – "對方說話或播放音樂時會跳動"
  - 送給對方 – "對方說話時應該幾乎不動，代表回音被消掉了"
* InfoBars (only when relevant): VB-Cable missing (error, action
  "下載 VB-Cable"); mic too loud (warning, "請把 Windows 的麥克風音量調低到
  約 70–80%", action "開啟音效設定"); pipeline error / device unplugged
  (error, message in plain words, action "重新開始"); auto start failed;
  Siri guard enabled but VB-CABLE A+B missing (warning).

## 裝置

* 麥克風 (settings card + combo box, description "你實際說話用的麥克風").
* 喇叭 (description "你聽對方聲音用的喇叭；程式會擷取它正在播的聲音來消除回音").
* VB-Cable card: status line ("已安裝，Discord / LINE 的麥克風請選「回聲消除
  麥克風」") and one button "整理 VB-Cable…" (dialog with two checkboxes:
  改成好懂的名稱 / 停用用不到的 VB-Cable 裝置, each with its explanation);
  "還原 VB-Cable 原本的設定" inside an expander.
* Expander "手動指定虛擬音效線（通常不用）": the two VB-Cable endpoints.

## Hey Siri 防誤觸

* Explanation card: "家裡有 HomePod / iPhone 嗎？通話對方說「Hey Siri」時，
  你的喇叭會把它播出來而叫醒 Siri。開啟後會在對方的聲音裡削掉 Siri
  聽得到的頻段。" + link to the detailed doc.
* Toggle 啟用.
* Requirement InfoBar: needs VB-CABLE A+B (download link) and the call
  app's output device must be "Siri防護喇叭" (copy button).
* 模式: radio buttons with descriptions — always（推薦，聲音稍微悶一點，
  不增加延遲）/ sibilant（只在 /s/ 音時處理，對方聲音延遲約 1 秒）/
  detect（需要喚醒詞模型檔，延遲約 1 秒）.  detect shows a model file
  picker and a 靈敏度 slider (threshold; higher = less sensitive).
* 強度: combo box with descriptions of each profile.

## 進階

* 回聲消除引擎: radio buttons — 自動 / WebRTC（推薦）/ Speex（雙方同時講話
  比較不會被吃字）/ NLMS（備援）/ 關閉（只轉送）, each with one line.
* 聲音處理 toggles: 降噪、去除低頻嗡嗡聲（高通）、自動音量（AGC）.
* 延遲 card: "量測延遲" button (plays ~4 s of noise; result shown in an
  InfoBar with a one-click "套用建議值"), 喇叭延遲 / 麥克風延遲 number
  boxes, 輸出音量 slider (dB).
* 啟動: "登入 Windows 時自動啟動" toggle; "關閉視窗時縮到系統匣" note.
* 診斷 expander: live technical numbers (buffer ms, drift ppm, underruns,
  ms per frame), debug recording folder picker, log view (last 300 lines)
  with "複製記錄" (for sending to the author), "開啟設定檔資料夾".
* About: version, licence, Slint attribution (required by the Slint
  royalty-free licence), link to GitHub.

## 說明

* Same content as `docs/getting-started.md` (rendered natively, not a web
  view): the 5-step guide, how to check it works ("請對方說話，『送給對方』
  幾乎不動 = 成功"), FAQ / troubleshooting:
  - 對方還是聽得到回音
  - 對方說我的聲音斷斷續續 / 被吃字
  - 對方完全聽不到我
  - 開機後沒有自動開始
  - 想完全移除
* Buttons: "重新執行設定精靈", "開啟線上說明".

## Tray

Icon with tooltip "Desk Echo Guard（運作中 / 已停止）"; menu: 顯示視窗
(default, double-click), 開始 / 停止, 結束.
