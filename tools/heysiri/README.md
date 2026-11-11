# 「Hey Siri」偵測模型的訓練工具

`heysiri` 模式用的小型神經網路（`crates/dsp/src/heysiri.bin`，約 230 KB）是用這裡的腳本訓練的。一般使用者不需要碰這些；只有想重新訓練模型時才需要。

## 做法

- 特徵由 Rust 的前端自己算（`cargo run --release -p echoguard-dsp --example heysiri_dump`），所以訓練和執行時看到的輸入完全一樣。
- 每個「s」音開頭是一個樣本。三個階段各訓練一個小 CNN（約 2 萬個參數）：
  - 階段 0：「s」音之前的 0.4 秒像不像「Hey」。
  - 階段 1、2：0.12 秒、0.25 秒後，聽起來還像不像「Hey Si…」「Hey Sir…」。
- 資料依語者／語音／錄音分成訓練、驗證、測試三份；門檻設成驗證資料裡 98.5–99% 的「Hey Siri」會通過（但其他「s」音的通過率不超過 10% / 5% / 3%）。
- 強度「加強」（`early`）多一關「Hey」（`train_hey.py` → `crates/dsp/src/heysiri_hey.bin`，約 48 KB）：每一框都算一次，判斷最近 0.25 秒是不是剛講到「Hey」的元音開頭；門檻設成驗證資料的其他語音每分鐘觸發 3 次。

## 資料（全部是公開資料，放在 `<workdir>/data`）

| 資料 | 用途 | 授權 |
|---|---|---|
| [Piper 語音](https://huggingface.co/rhasspy/piper-voices)（`libritts_r` 904 人、`vctk` 109 人、`l2arctic` 24 人、`arctic` 18 人、8 個單人英文語音、3 個中文語音） | 合成「Hey Siri」、「Hey Sam」這類相近的句子、中英文句子 | 各語音的授權見其 MODEL_CARD |
| [TigreGotico/synthetic-wakeword-hey_siri](https://huggingface.co/datasets/TigreGotico/synthetic-wakeword-hey_siri) | 其他語音合成引擎念的「hey siri」（1288 段） | CC-BY-4.0 |
| [google/fleurs](https://huggingface.co/datasets/google/fleurs)（`cmn_hans_cn`、`yue_hant_hk`、`en_us`） | 真人朗讀的中文、粵語、英文，當作「不該觸發」的例子 | CC-BY-4.0 |

- 沒有真人說「Hey Siri」的錄音（找不到公開可用的），正例全部是合成語音。
- 一部分音檔會加上噪音、窄頻、Opus 編碼（模擬通話）。

## 步驟

```bash
python -m venv venv && ./venv/bin/pip install numpy scipy soundfile "piper-tts[zh]" huggingface_hub
./venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
# 下載上表的資料到 <workdir>/data/{piper,tigre,fleurs}（FLEURS 的 tar.gz 要解壓縮），需要 ffmpeg
./venv/bin/python gen_data.py <workdir> 4          # 產生音檔（4 核心約 1 小時、約 25 GB）
cargo build --release -p echoguard-dsp --example heysiri_dump
./venv/bin/python train.py <workdir> target/release/examples/heysiri_dump /tmp/eval.bin        # 保留測試集、印出結果（約 15 分鐘）
./venv/bin/python train.py <workdir> target/release/examples/heysiri_dump crates/dsp/src/heysiri.bin --all   # 出貨用的模型
./venv/bin/python train_hey.py <workdir> target/release/examples/heysiri_dump /tmp/hey_eval.bin              # 「Hey」那一關：保留測試集
./venv/bin/python train_hey.py <workdir> target/release/examples/heysiri_dump crates/dsp/src/heysiri_hey.bin --all
```

結果數字見 [docs/siri-guard-research.md](../../docs/siri-guard-research.md#heysiri-模式的偵測器)。
