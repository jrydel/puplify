//! Whisper (ggml) model catalog and downloader.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::paths;

/// `(name, approximate size in MB)`. Names map to
/// `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-<name>.bin`.
/// `-q5_*` variants are quantized: a third of the size, practically the same accuracy.
pub const CATALOG: &[(&str, u32)] = &[
    ("base-q5_1", 57),
    ("base.en-q5_1", 57),
    ("small-q5_1", 181),
    ("small.en-q5_1", 181),
    ("medium-q5_0", 514),
    ("large-v3-turbo-q5_0", 547),
    ("tiny", 75),
    ("tiny.en", 75),
    ("base", 142),
    ("base.en", 142),
    ("small", 466),
    ("small.en", 466),
    ("medium", 1_530),
    ("medium.en", 1_530),
    ("large-v3-turbo", 1_620),
];

/// Model shipped inside the desktop installers, used until the user picks another.
pub const BUNDLED_MODEL: &str = "base-q5_1";

/// Read-only directory with models shipped inside the app bundle (set by the
/// desktop app at startup). Models downloaded by the user take precedence.
static BUNDLED_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_bundled_dir(dir: PathBuf) {
    let _ = BUNDLED_DIR.set(dir);
}

/// Downloaded copy first, then the copy shipped with the app.
fn locate(file_name: &str) -> Result<Option<PathBuf>> {
    let downloaded = paths::models_dir()?.join(file_name);
    if downloaded.is_file() {
        return Ok(Some(downloaded));
    }
    Ok(BUNDLED_DIR.get().map(|dir| dir.join(file_name)).filter(|p| p.is_file()))
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub name: &'static str,
    pub size_mb: u32,
    /// Ready to use: downloaded or shipped with the app.
    pub downloaded: bool,
    /// Shipped with the app; nothing to download.
    pub bundled: bool,
}

fn file_name(name: &str) -> String {
    format!("ggml-{name}.bin")
}

pub fn list() -> Result<Vec<ModelStatus>> {
    let bundled_dir = BUNDLED_DIR.get();
    CATALOG
        .iter()
        .map(|&(name, size_mb)| {
            let bundled = bundled_dir.is_some_and(|d| d.join(file_name(name)).is_file());
            Ok(ModelStatus { name, size_mb, downloaded: locate(&file_name(name))?.is_some(), bundled })
        })
        .collect()
}

/// Resolves a configured model (catalog name or path to a `.bin`) to a file on disk.
pub fn resolve(model: &str) -> Result<PathBuf> {
    let as_path = Path::new(model);
    if as_path.is_absolute() {
        if as_path.is_file() {
            return Ok(as_path.to_path_buf());
        }
        bail!("whisper model file {} does not exist", as_path.display());
    }
    if !CATALOG.iter().any(|(n, _)| *n == model) {
        bail!("unknown whisper model {model:?}; use one of the catalog names or an absolute path");
    }
    locate(&file_name(model))?.with_context(|| {
        format!("whisper model {model} is not downloaded yet (download it in Settings or run `puplify models download {model}`)")
    })
}

/// Silero voice-activity model used by whisper.cpp to skip non-speech audio,
/// which prevents hallucinated text on background noise.
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Returns the VAD model, downloading it (≈0.9 MB) if neither a downloaded
/// nor a bundled copy exists.
pub async fn ensure_vad() -> Result<PathBuf> {
    if let Some(path) = locate(VAD_FILE)? {
        return Ok(path);
    }
    let dest = paths::models_dir()?.join(VAD_FILE);
    fetch(VAD_URL, &dest, |_, _| {}).await.context("downloading the voice-activity model")?;
    Ok(dest)
}

/// Downloads a catalog model (and the VAD model), reporting `(downloaded_bytes, total_bytes)`.
pub async fn download(name: &str, progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    if !CATALOG.iter().any(|(n, _)| *n == name) {
        bail!("unknown whisper model {name:?}");
    }
    ensure_vad().await?;
    let dest = paths::models_dir()?.join(file_name(name));
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", file_name(name));
    fetch(&url, &dest, progress).await?;
    Ok(dest)
}

async fn fetch(url: &str, dest: &Path, mut progress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let part = dest.with_extension("bin.part");
    let resp = reqwest::get(url).await?.error_for_status().with_context(|| format!("downloading {url}"))?;
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(&part).await?;
    let mut done = 0u64;
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.context("download interrupted")?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    file.flush().await?;
    drop(file);
    if let Some(total) = total {
        if done != total {
            bail!("download incomplete ({done} of {total} bytes)");
        }
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}
