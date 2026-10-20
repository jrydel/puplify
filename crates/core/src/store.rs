//! Local transcript storage: one JSON file per transcript in the data directory.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{paths, transcript::Transcript};

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSummary {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: u64,
    pub segments: usize,
}

fn path_for(id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("invalid transcript id {id:?}");
    }
    Ok(paths::transcripts_dir()?.join(format!("{id}.json")))
}

pub fn save(t: &Transcript) -> Result<()> {
    let json = serde_json::to_vec_pretty(t)?;
    paths::write_atomic(&path_for(&t.id)?, &json, false)
}

pub fn load(id: &str) -> Result<Transcript> {
    let path = path_for(id)?;
    let bytes = std::fs::read(&path).with_context(|| format!("transcript {id} not found"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

pub fn delete(id: &str) -> Result<()> {
    std::fs::remove_file(path_for(id)?).with_context(|| format!("deleting transcript {id}"))
}

/// All stored transcripts, newest first. Unreadable files are skipped with a warning.
pub fn list() -> Result<Vec<TranscriptSummary>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(paths::transcripts_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Transcript>(&b)?));
        match parsed {
            Ok(t) => out.push(TranscriptSummary {
                duration_ms: t.duration_ms(),
                segments: t.segments.len(),
                id: t.id,
                title: t.title,
                started_at: t.started_at,
                ended_at: t.ended_at,
            }),
            Err(e) => tracing::warn!("skipping {}: {e:#}", path.display()),
        }
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(out)
}
