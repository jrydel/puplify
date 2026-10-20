//! Whisper (ggml) model catalog and downloader.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::paths;

/// `(name, approximate size in MB)`. Names map to
/// `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-<name>.bin`.
pub const CATALOG: &[(&str, u32)] = &[
    ("tiny.en", 75),
    ("tiny", 75),
    ("base.en", 142),
    ("base", 142),
    ("small.en", 466),
    ("small", 466),
    ("medium.en", 1_530),
    ("medium", 1_530),
    ("large-v3-turbo-q5_0", 547),
    ("large-v3-turbo", 1_620),
];

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub name: &'static str,
    pub size_mb: u32,
    pub downloaded: bool,
}

fn file_for(name: &str) -> Result<PathBuf> {
    Ok(paths::models_dir()?.join(format!("ggml-{name}.bin")))
}

pub fn list() -> Result<Vec<ModelStatus>> {
    CATALOG
        .iter()
        .map(|&(name, size_mb)| Ok(ModelStatus { name, size_mb, downloaded: file_for(name)?.exists() }))
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
    let path = file_for(model)?;
    if !path.is_file() {
        bail!("whisper model {model} is not downloaded yet (download it in Settings or run `puplify models download {model}`)");
    }
    Ok(path)
}

/// Silero voice-activity model used by whisper.cpp to skip non-speech audio,
/// which prevents hallucinated text on background noise.
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

pub fn vad_path() -> Result<PathBuf> {
    Ok(paths::models_dir()?.join("ggml-silero-v5.1.2.bin"))
}

/// Downloads the VAD model if it is missing (≈0.9 MB).
pub async fn ensure_vad() -> Result<PathBuf> {
    let dest = vad_path()?;
    if !dest.is_file() {
        fetch(VAD_URL, &dest, |_, _| {}).await.context("downloading the voice-activity model")?;
    }
    Ok(dest)
}

/// Downloads a catalog model (and the VAD model), reporting `(downloaded_bytes, total_bytes)`.
pub async fn download(name: &str, progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    if !CATALOG.iter().any(|(n, _)| *n == name) {
        bail!("unknown whisper model {name:?}");
    }
    ensure_vad().await?;
    let dest = file_for(name)?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin");
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
