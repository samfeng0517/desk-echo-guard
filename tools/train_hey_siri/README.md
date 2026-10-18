# 訓練自訂「Hey Siri」偵測模型（openWakeWord）

`--siri-mode detect` 需要一個能偵測「Hey Siri」的 openWakeWord 模型。openWakeWord
官方沒有提供這個詞的預訓練模型，所以要自己訓練。**本專案沒有實際訓練或測試過這個模型**
（雲端環境沒有 GPU，且訓練資料需要下載數十 GB），以下步驟依 openWakeWord 官方文件整理。

> 不想訓練？改用 `--siri-mode sibilant`（不需模型，偵測 /s/ 音後陷波）或
> `--siri-mode always`（全程套用陷波）。

## 方法一：Google Colab（最簡單，約 1 小時以內）

1. 打開 openWakeWord 官方的 Colab 筆記本：
   <https://colab.research.google.com/drive/1q1oe2zOyZp7UsB3jJiQ1IFn8z5YfjwEb?usp=sharing>
2. `target_word` 填 `hey siri`（可再訓練一個 `siri`）。
3. 依指示執行全部儲存格，最後下載 `hey_siri.onnx`（或 `.tflite`）。
4. 官方提醒：這個快速版本在某些環境下準確度可能偏低。

## 方法二：完整訓練流程（品質較好，需要 Linux + NVIDIA GPU）

依官方 `notebooks/automatic_model_training.ipynb`
（<https://github.com/dscripka/openWakeWord/blob/main/notebooks/automatic_model_training.ipynb>）：

```bash
git clone https://github.com/dscripka/openWakeWord
pip install -e ./openWakeWord[full]
git clone https://github.com/rhasspy/piper-sample-generator   # 合成正樣本用的 TTS
# 依 notebook 下載：MIT 房間脈衝響應 (mit_rirs)、AudioSet / FMA 背景音、
# ACAV100M 預先計算的負樣本特徵 (openwakeword_features_ACAV100M_2000_hrs_16bit.npy，約 16 GB)
# 以及 validation_set_features.npy
cp hey_siri.yml ./           # 本資料夾的設定檔
python openWakeWord/openwakeword/train.py --training_config hey_siri.yml --generate_clips
python openWakeWord/openwakeword/train.py --training_config hey_siri.yml --augment_clips
python openWakeWord/openwakeword/train.py --training_config hey_siri.yml --train_model
# 產出 hey_siri_model/hey_siri.onnx
```

`hey_siri.yml` 已經加入幾個容易混淆的負樣本片語（"hey series"、"seriously"…），
避免一般對話被誤判而被不必要地陷波。

## 驗證與選門檻

錄幾段通話中出現「Hey Siri」的音檔、和幾段一般對話，執行：

```bash
python tools/train_hey_siri/check_model.py hey_siri.onnx heysiri1.wav talk1.wav ...
```

真的「Hey Siri」分數應明顯高於 `--siri-threshold`（預設 0.5），一般對話應遠低於它。

## 使用

```bash
python -m echoguard --siri-guard --siri-mode detect --siri-model hey_siri.onnx --siri-threshold 0.5
```

偵測器在整句話**講完後**才會觸發，所以 Siri 防護會把通話聲音延遲約 1 秒
（`--siri-lookahead-s`），讓觸發時那一句仍在緩衝區裡、還來得及陷波後才播出。
