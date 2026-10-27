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

Left navigation pane (icons + labels, collapsible), four items plus a link:

1. **首頁** – status and the one big button
2. **裝置** – microphone, speakers, VB-Cable
3. **Hey Siri 防誤觸**
4. **進階** – sound tuning, delay measurement, start-up, logs
5. 說明文件 (link, opens the online guide)

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

## Tooltips (instead of a setup wizard)

There is no setup wizard and no tutorial screens.  Help comes from
**hover tooltips**: every control, settings card, meter, InfoBar action and
nav item shows a Fluent-style tooltip after ~500 ms of hover (and on
keyboard focus), disappearing on leave.  A tooltip answers in 1–3 short
sentences: what it is, when to change it, the recommended value.  Examples:

* 麥克風: "你實際說話用的麥克風。通常用 Windows 預設就好。"
* 喇叭: "你聽對方聲音用的喇叭。程式會擷取它正在播的聲音，用來把回音扣掉。"
* 降噪: "去除風扇、冷氣等持續的背景聲。建議開啟。"
* 送給對方 meter: "對方聽到的聲音。對方說話時這條幾乎不動，代表回音被消掉了。"
* 開始: "開始消除回音。通話前先按這裡，再開 Discord / LINE。"

Slint has no built-in tooltip widget: implement one reusable `ToolTip`
component (hover delay timer, rounded card with shadow, positioned below
the target and kept inside the window, max width ~320 px, wraps text).
Keep all tooltip texts in one place (a `.slint` global or a Rust table) so
they are easy to review and edit.  A tooltip must disappear even if its
text or position changed while it was up (開始 -> 停止 when the run state
changes, a window resize): the Tip that showed it owns it by a serial
number, not by matching text and position.  It also disappears when the
window loses focus, is minimized or hidden to the tray, or is resized.

Window repaints: the default software renderer only repaints what changed,
but Windows may throw the window contents away (minimize / restore, hide /
show, lock screen, display changes; seen on Windows 10), which left stale
pixels: the windows behind it, old tooltips.  On those window events
(focus gained, un-occluded, resized, scale change) and on every show, the
controller flips `AppState.repaint`, which dirties a full-window invisible
rectangle, so the next frame repaints everything (again 200 ms later, after
the restore animation).

## 首頁

* Hero card: large shield icon, state title "回聲消除：運作中" / "已停止",
  one sentence ("對方聽不到自己的回音了。" / "按「開始」後，對方就不會
  聽到回音。"), accent button **開始** / **停止**.
* "在通話軟體這樣設定" card: "麥克風（輸入裝置）選：**回聲消除麥克風**"
  [複製]; when the Siri guard is on also "喇叭（輸出裝置）選：**Siri防護喇叭**".
  Tooltip on each line explains where that setting is in Discord / LINE.
* Live levels card, three meters with plain names and one-line hints:
  - 你的麥克風 – "你說話時會跳動"
  - 喇叭播放中 – "對方說話或播放音樂時會跳動"
  - 送給對方 – "對方說話時應該幾乎不動，代表回音被消掉了"
* InfoBars (only when relevant): VB-Cable missing (error, action
  "下載 VB-Cable"); mic too loud (warning, "請把 Windows 的麥克風音量調低到
  約 70–80%", action "開啟音效設定"); waiting for a device to come back
  (info "等音效裝置回來", action "立即重試", see below); pipeline error
  (error, message in plain words, action "重新開始"); auto start failed;
  Siri guard enabled but VB-CABLE A+B missing (warning).
* Recovery without clicks: when the pipeline stops by itself (sleep, the
  display turned off with its speakers, a device unplugged) the window
  does not pop up; the controller waits until the devices it needs are
  present (a "Windows 預設" speaker must not be VB-Cable) and restarts:
  2 s after each device change, else every 5 s for a minute, then every
  30 s.  Only after 5 failures in a row within 15 s of starting does it
  show the "回聲消除停止了" error.  While running on a "Windows 預設"
  device, a change of that Windows default restarts on the new one.

## 裝置

* 麥克風 (settings card + combo box, description "你實際說話用的麥克風").
* 喇叭 (description "你聽對方聲音用的喇叭；程式會擷取它正在播的聲音來消除回音").
* VB-Cable card (when VB-Cable is missing: InfoBar with "下載 VB-Cable" and a
  tooltip listing the install steps: 解壓縮 → 以系統管理員身分執行
  VBCABLE_Setup_x64.exe → Install Driver → 重新開機): status line ("已安裝，Discord / LINE 的麥克風請選「回聲消除
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
  不增加延遲）/ sibilant（只在 /s/ 音時處理約半秒，延遲只多約 0.06 秒；
  其他有 /s/ 音的字也會被短暫處理）.  (The wake-word detect mode with its
  model picker and threshold slider was removed in 0.4.1.)
* 強度: combo box with descriptions of each profile.  標準 (deep-wide) is
  described as the strongest in Siri's band but making consonants less
  clear, pointing to 輕微 (apple-deep) if speech becomes hard to understand.

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

## 說明 (no separate page)

The nav pane ends with a "說明文件" link that opens the online guide
(`docs/getting-started.md` on GitHub).  Everything else is in tooltips.

## Tray

Icon with tooltip "Desk Echo Guard（運作中 / 已停止）"; menu: 顯示視窗
(default, double-click), 開始 / 停止, 結束.
