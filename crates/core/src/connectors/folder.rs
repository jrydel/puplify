use anyhow::{bail, Context, Result};

use crate::config::FolderConfig;
use crate::transcript::Transcript;

/// Writes the transcript in each configured format; returns the paths written.
pub async fn export(cfg: &FolderConfig, transcript: &Transcript) -> Result<String> {
    if cfg.path.as_os_str().is_empty() {
        bail!("export folder is not set");
    }
    if cfg.formats.is_empty() {
        bail!("no export formats selected");
    }
    tokio::fs::create_dir_all(&cfg.path)
        .await
        .with_context(|| format!("creating {}", cfg.path.display()))?;
    let stem = transcript.file_stem();
    let mut written = Vec::new();
    for &format in &cfg.formats {
        let path = cfg.path.join(format!("{stem}.{}", format.extension()));
        tokio::fs::write(&path, transcript.render(format))
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        written.push(path.display().to_string());
    }
    Ok(written.join(", "))
}
