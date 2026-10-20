//! Locating / downloading openWakeWord's shared feature models
//! (`siri_guard._feature_model_paths`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

pub const MELSPEC_FILE: &str = "melspectrogram.onnx";
pub const EMBEDDING_FILE: &str = "embedding_model.onnx";
/// `openwakeword.FEATURE_MODELS[...]["download_url"]` with `.onnx`.
pub const RELEASE_URL: &str = "https://github.com/dscripka/openWakeWord/releases/download/v0.5.1";

/// Per-user model cache: `%LOCALAPPDATA%\desk-echo-guard\models` on
/// Windows, `$XDG_CACHE_HOME` or `~/.cache/desk-echo-guard/models` elsewhere.
pub fn cache_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join("desk-echo-guard").join("models"))
}

pub(crate) fn reject_tflite(model_paths: &[PathBuf]) -> Result<()> {
    for p in model_paths {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("onnx") => {}
            Some("tflite") => bail!(
                "Rust 版只支援 ONNX 模型，不支援 TFLite：{}\n請改用同一模型匯出的 .onnx 檔（訓練工具會同時輸出 .onnx）。",
                p.display()
            ),
            _ => bail!("喚醒詞模型必須是 .onnx 檔：{}", p.display()),
        }
    }
    Ok(())
}

/// Paths of `melspectrogram.onnx` and `embedding_model.onnx`: next to one of
/// the wake-word models, else in the per-user cache, downloading missing
/// files there.
pub fn feature_model_paths(model_paths: &[PathBuf]) -> Result<(PathBuf, PathBuf)> {
    reject_tflite(model_paths)?;
    let mut dirs: Vec<PathBuf> = model_paths
        .iter()
        .filter_map(|p| std::path::absolute(p).ok()?.parent().map(Path::to_path_buf))
        .collect();
    let cache = cache_dir();
    dirs.extend(cache.clone());
    let find = |name: &str| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
    if let (Some(m), Some(e)) = (find(MELSPEC_FILE), find(EMBEDDING_FILE)) {
        return Ok((m, e));
    }
    let cache =
        cache.ok_or_else(|| anyhow!("找不到使用者快取資料夾（LOCALAPPDATA / HOME 未設定）"))?;
    let mut out = Vec::new();
    for name in [MELSPEC_FILE, EMBEDDING_FILE] {
        let dest = cache.join(name);
        if !dest.is_file() {
            let url = format!("{RELEASE_URL}/{name}");
            download(&url, &dest).map_err(|e| {
                anyhow!(
                    "無法下載 openWakeWord 特徵模型 {name}：{e:#}\n\
                     請手動從 {url} 下載，並放到 {} 或喚醒詞模型（.onnx）所在的資料夾。",
                    cache.display()
                )
            })?;
        }
        out.push(dest);
    }
    let e = out.pop().unwrap_or_default();
    let m = out.pop().unwrap_or_default();
    Ok((m, e))
}

/// Download `url` to `dest` (via a temporary file, so an interrupted
/// download never leaves a truncated model behind).  Honours HTTPS_PROXY and
/// the OS certificate store.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("無法建立資料夾 {}", d.display()))?;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| anyhow!("{e}"))?;
    let mut data = Vec::new();
    resp.into_body()
        .into_with_config()
        .limit(64 << 20)
        .reader()
        .read_to_end(&mut data)?;
    if data.len() < 1024 {
        bail!("下載內容太小（{} bytes）", data.len());
    }
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &data).with_context(|| format!("無法寫入 {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("無法寫入 {}", dest.display()))?;
    Ok(())
}
