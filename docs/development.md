# 開發說明

[← 回 README](../README.md)

## 測試

```bash
pip install -r requirements-dev.txt
pytest -s        # -s 會印出每項的量測數字
```

| 測試檔 | 內容 |
|---|---|
| `tests/test_aec.py` | 各引擎 ERLE、近端保留度、雙講 |
| `tests/test_buffers.py` | 時脈漂移、欠載時保持對齊 |
| `tests/test_siri_guard.py` | 陷波頻響、頻譜效果、detect 模式時序 |
| `tests/test_app_sim.py` | 完整多執行緒程式接模擬裝置（各裝置時脈不同） |
| `tests/test_cli.py` | 設定檔、離線 `--sim-*` |
| `tests/test_gui.py` | 設定視窗啟動 / 停止 / 存讀設定。需要 tkinter 和螢幕：沒有就跳過，Linux 上可用 `xvfb-run` |

設環境變數 `ECHOGUARD_REPORT=path.txt`，就會把量測數字附加寫到這個檔案。

`experiments/` 是需要網路的評估腳本。它會下載 Piper TTS 語音和 openWakeWord 模型：

```bash
pip install piper-tts openwakeword onnxruntime pystoi
python experiments/aec_tts_eval.py --cache .cache      # 引擎在 TTS 語音上的表現
python experiments/guard_proxy_eval.py --cache .cache  # Siri 陷波對開源喚醒詞偵測器的效果
```

## 打包 exe

工作流程檔：`.github/workflows/build-exe.yml`。

**每次推到 `main` 會自動執行**

- 在 Windows 上跑全部測試。
- 用 PyInstaller 打包成單一 exe（`packaging/desk_echo_guard.py` 是進入點）。
- 跑 exe 的 `--self-test`。
- 把 exe 上傳到該次 Actions 執行的 Artifacts。

**發佈新版本**

- 方法一：在 Actions 頁面手動執行「Build Windows exe」，`release_tag` 填版本號（例如 `v0.2.0`）。
- 方法二：推一個 `v*` 標籤。
- 兩種方法都會建立 GitHub Release，並附上 exe。

**刪除某個版本**：手動執行「Delete release」並填標籤名稱，Release 和標籤會一起刪掉。

**本機打包**：在 Windows 上執行 `packaging\build_exe.bat`。

**更換圖示**：修改 `tools/make_icon.py` 後執行它，會重新產生 `echoguard/assets/` 裡的 `.ico` / `.png`（需要 Pillow）。

## Hey Siri 喚醒詞模型

見 [tools/train_hey_siri](../tools/train_hey_siri/README.md)。
