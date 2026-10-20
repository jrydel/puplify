use std::path::PathBuf;

use anyhow::{Context, Result};

/// Environment variable that relocates all config and data under one directory.
/// Useful for portable installs and tests.
pub const HOME_ENV: &str = "PUPLIFY_HOME";

fn dirs() -> Result<(PathBuf, PathBuf)> {
    if let Some(home) = std::env::var_os(HOME_ENV) {
        let home = PathBuf::from(home);
        return Ok((home.join("config"), home.join("data")));
    }
    let dirs = directories::ProjectDirs::from("", "", "puplify")
        .context("could not determine home directory")?;
    Ok((dirs.config_dir().to_path_buf(), dirs.data_dir().to_path_buf()))
}

fn ensure(path: PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    Ok(path)
}

/// `~/.config/puplify` on Linux, `~/Library/Application Support/puplify` on macOS.
pub fn config_dir() -> Result<PathBuf> {
    ensure(dirs()?.0)
}

/// `~/.local/share/puplify` on Linux, `~/Library/Application Support/puplify` on macOS.
pub fn data_dir() -> Result<PathBuf> {
    ensure(dirs()?.1)
}

pub fn transcripts_dir() -> Result<PathBuf> {
    ensure(data_dir()?.join("transcripts"))
}

pub fn models_dir() -> Result<PathBuf> {
    ensure(data_dir()?.join("models"))
}

/// Write `contents` to `path` atomically (temp file + rename). When `private`
/// is set the file is created with mode 0600 because it holds secrets.
pub fn write_atomic(path: &std::path::Path, contents: &[u8], private: bool) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
