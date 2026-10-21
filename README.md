<img src="echoguard/assets/icon.png" width="72" align="right" alt="">

# desk-echo-guard

Windows 用的全系統回聲消除程式，給用**桌上型喇叭 + 桌上型麥克風**講 Discord / LINE 通話的人。

- **回聲消除**：對方不會再從你的麥克風聽到自己的回音。程式擷取喇叭正在播的聲音當參考，用 WebRTC AEC3 從麥克風訊號裡扣掉，再透過 VB-Cable 當成虛擬麥克風給通話軟體用。
- **Hey Siri 防誤觸**（選用）：對方說「Hey Siri」時，不會叫醒你的 HomePod / iPhone。做法仿照 Apple 在 WWDC 2026 被觀察到的手法：只在講到「Siri」時削掉幾個窄頻帶，不是靜音。

> **使用前請知道**
> - v0.4.0 起是用 Rust 全新改寫的版本：更省記憶體與 CPU、單一 exe 秒開、Windows 11 風格介面，每個選項都有滑鼠提示。它通過了所有自動測試與 Windows 自我檢測，但還沒在實機上長時間使用過；遇到問題可以先改用 [v0.3.1](https://github.com/samfeng0517/desk-echo-guard/releases/tag/v0.3.1)（設定檔共用）並開 issue 回報。
> - 舊版（Python，v0.3.1）已在 Windows 實機（桌上型喇叭 + 桌上型麥克風，Discord 通話）上使用，回聲消除有效；新版用的是同一套 WebRTC 回聲消除。
> - Hey Siri 防誤觸預設用 `deep-wide`（2.8–6.5 kHz 整段深陷波）；舊版較淺的 `apple` 實機擋不住，已移除。Apple 的做法沒有官方公開，詳見 [Hey Siri 防誤觸](docs/siri-guard.md)。

## 快速開始

第一次使用、或要教朋友設定，請看圖文版的 **[5 分鐘上手](rust/docs/getting-started.md)**；遇到問題看 **[常見問題](rust/docs/faq.md)**。

1. **安裝 VB-Cable**：從 <https://vb-audio.com/Cable/> 安裝後重新開機（[詳細步驟](docs/setup.md#1-安裝-vb-cable必要)）。
2. **下載程式**：到 [Releases](https://github.com/samfeng0517/desk-echo-guard/releases) 下載 `DeskEchoGuard.exe`，不用裝 Python。
   - Windows 若顯示「已保護您的電腦」，按「其他資訊 → 仍要執行」。
3. **設定通話軟體**（[詳細步驟](docs/setup.md#3-通話軟體與音效設定)）：
   - 麥克風改選 **`CABLE Output`**。在程式的「裝置」頁按「整理 VB-Cable…」之後，它會改叫 **`回聲消除麥克風`**（[說明](docs/setup.md#讓-vb-cable-的名稱好懂一點建議)）。
   - 關掉通話軟體自己的回音消除。噪音抑制可以開（Discord 建議選 Krisp，可以去除鍵盤聲）。
   - 關掉 NVIDIA Broadcast。
4. **開啟 `DeskEchoGuard.exe`**：麥克風和喇叭預設用 Windows 的預設裝置，VB-Cable 會自動找到，直接在首頁按「開始」就好。滑鼠移到任何選項上都會出現說明。之後每次都**先開程式再開通話**。

## 文件

| 文件 | 內容 |
|---|---|
| [5 分鐘上手](rust/docs/getting-started.md) | 給第一次用的人：一步一步設定 VB-Cable、Discord、LINE，怎麼確認有效 |
| [常見問題](rust/docs/faq.md) | 對方還是聽到回音、聲音斷斷續續、聽不到我… 依症狀找解法 |
| [新版說明與編譯](rust/README.md) | Rust 版的功能、從原始碼編譯、授權 |
| [安裝與設定](docs/setup.md) | VB-Cable、exe 或原始碼安裝、Discord / LINE / NVIDIA Broadcast 設定、麥克風音量 |
| [使用說明與調校](docs/usage.md) | 設定視窗、命令列與設定檔、常見問題怎麼調 |
| [Hey Siri 防誤觸](docs/siri-guard.md) | 怎麼選設定、Apple 做法的研究摘要與來源、實測效果 |
| [運作原理](docs/how-it-works.md) | 接線圖、時脈漂移補償、引擎比較、延遲、程式碼結構 |
| [驗證結果與限制](docs/verification.md) | 量測數據、實機使用結果、已知限制 |
| [開發說明](docs/development.md) | 測試、評估腳本、打包 exe 與發佈版本（Python 版） |

## 授權

本專案以 [MIT 授權](LICENSE)釋出。exe 內含的第三方元件（Slint、WebRTC、SpeexDSP 等）各自依其原本的授權使用，詳見 [rust/README.md](rust/README.md#授權)。VB-Cable 是 VB-Audio 的軟體，需要另外安裝，不包含在本專案內。
