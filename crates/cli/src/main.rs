use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use puplify_core::{
    audio,
    config::{Config, ExportFormat},
    connectors::{self, dropbox, google_drive, onedrive, webhook},
    session::{self, Recording, SessionEvent},
    store,
    stt::models,
    transcript::fmt_ts,
};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "puplify", version, about = "Meeting transcription with connectors (OneDrive, Google Drive, Dropbox, Notion, webhooks, folders)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record microphone + system audio and transcribe live. Ctrl-C stops.
    Record {
        #[arg(short, long, default_value = "")]
        title: String,
        /// Stop automatically after this many seconds.
        #[arg(short, long)]
        duration: Option<u64>,
    },
    /// Transcribe a WAV file with the configured engine and deliver it.
    Transcribe {
        file: PathBuf,
        #[arg(short, long)]
        title: Option<String>,
    },
    /// List stored transcripts.
    List,
    /// Print a transcript.
    Show {
        id: String,
        #[arg(short, long, value_enum, default_value_t = Format::Txt)]
        format: Format,
    },
    /// Re-run all enabled connectors for a stored transcript.
    Deliver { id: String },
    /// Delete a stored transcript.
    Delete { id: String },
    /// List audio capture devices.
    Devices,
    /// Manage local Whisper models.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    /// Connect or disconnect OneDrive.
    Onedrive {
        #[command(subcommand)]
        command: OneDriveCommand,
    },
    /// Connect or disconnect Google Drive.
    Gdrive {
        #[command(subcommand)]
        command: SignInCommand,
    },
    /// Connect or disconnect Dropbox.
    Dropbox {
        #[command(subcommand)]
        command: SignInCommand,
    },
    /// Send a test event to a configured webhook (by name or URL; all when omitted).
    WebhookTest { name: Option<String> },
    /// Print the config file path and contents.
    Config,
}

#[derive(Subcommand)]
enum ModelsCommand {
    List,
    Download { name: String },
}

#[derive(Subcommand)]
enum OneDriveCommand {
    Login,
    Status,
    Logout,
}

#[derive(Subcommand)]
enum SignInCommand {
    Login,
    Status,
    Logout,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Txt,
    Md,
    Json,
}

impl From<Format> for ExportFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Txt => Self::Txt,
            Format::Md => Self::Md,
            Format::Json => Self::Json,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,whisper_rs=off".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.command {
        Command::Record { title, duration } => record(cfg, title, duration).await,
        Command::Transcribe { file, title } => {
            let title = title.unwrap_or_else(|| {
                file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            });
            let (tx, rx) = mpsc::unbounded_channel();
            let printer = tokio::spawn(print_events(rx));
            let result = session::transcribe_file(cfg, &file, title, tx).await;
            printer.await?;
            let t = result?;
            println!("\nSaved transcript {}", t.id);
            Ok(())
        }
        Command::List => {
            for t in store::list()? {
                println!(
                    "{}  {}  {:>8}  {:>4} segs  {}",
                    t.id,
                    t.started_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
                    fmt_ts(t.duration_ms),
                    t.segments,
                    t.title
                );
            }
            Ok(())
        }
        Command::Show { id, format } => {
            print!("{}", store::load(&id)?.render(format.into()));
            Ok(())
        }
        Command::Deliver { id } => {
            let t = store::load(&id)?;
            let results = connectors::deliver(&cfg, &t).await;
            if results.is_empty() {
                bail!("no connectors are enabled in {}", Config::path()?.display());
            }
            report(&results)
        }
        Command::Delete { id } => store::delete(&id),
        Command::Devices => {
            for d in audio::list_devices()? {
                let kind = if d.is_monitor { "system" } else { "input " };
                println!("{kind}  {}  ({})", d.id, d.name);
            }
            Ok(())
        }
        Command::Models { command: ModelsCommand::List } => {
            for m in models::list()? {
                let mark = if m.downloaded { "downloaded" } else { "" };
                println!("{:<22} {:>6} MB  {mark}", m.name, m.size_mb);
            }
            Ok(())
        }
        Command::Models { command: ModelsCommand::Download { name } } => {
            let mut last = 0u64;
            let path = models::download(&name, |done, total| {
                let mb = done / 1_000_000;
                if mb != last {
                    last = mb;
                    match total {
                        Some(t) => eprint!("\r{mb} / {} MB", t / 1_000_000),
                        None => eprint!("\r{mb} MB"),
                    }
                    let _ = std::io::stderr().flush();
                }
            })
            .await?;
            eprintln!("\nSaved {}", path.display());
            Ok(())
        }
        Command::Onedrive { command: OneDriveCommand::Login } => {
            let code = onedrive::begin_login(&cfg.connectors.onedrive).await?;
            println!("{}", code.message);
            let account = onedrive::finish_login(&cfg.connectors.onedrive, &code).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.onedrive.enabled {
                println!("Note: OneDrive export is disabled; set connectors.onedrive.enabled = true in {}", Config::path()?.display());
            }
            Ok(())
        }
        Command::Onedrive { command: OneDriveCommand::Status } => {
            match onedrive::status()? {
                Some(account) => println!("Signed in as {account}"),
                None => println!("Not signed in"),
            }
            Ok(())
        }
        Command::Onedrive { command: OneDriveCommand::Logout } => onedrive::logout(),
        Command::Gdrive { command: SignInCommand::Login } => {
            let code = google_drive::begin_login(&cfg.connectors.google_drive).await?;
            println!("{}", code.message);
            let account = google_drive::finish_login(&cfg.connectors.google_drive, &code).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.google_drive.enabled {
                println!("Note: Google Drive export is disabled; set connectors.google_drive.enabled = true in {}", Config::path()?.display());
            }
            Ok(())
        }
        Command::Gdrive { command: SignInCommand::Status } => {
            match google_drive::status()? {
                Some(account) => println!("Signed in as {account}"),
                None => println!("Not signed in"),
            }
            Ok(())
        }
        Command::Gdrive { command: SignInCommand::Logout } => google_drive::logout(),
        Command::Dropbox { command: SignInCommand::Login } => {
            let pending = dropbox::begin_login(&cfg.connectors.dropbox)?;
            println!("Open this URL, approve access, and paste the code Dropbox shows:\n\n{}\n", pending.url);
            print!("Code: ");
            std::io::stdout().flush()?;
            let mut code = String::new();
            std::io::stdin().read_line(&mut code).context("reading the code from stdin")?;
            let account = dropbox::finish_login(&cfg.connectors.dropbox, &pending.verifier, &code).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.dropbox.enabled {
                println!("Note: Dropbox export is disabled; set connectors.dropbox.enabled = true in {}", Config::path()?.display());
            }
            Ok(())
        }
        Command::Dropbox { command: SignInCommand::Status } => {
            match dropbox::status()? {
                Some(account) => println!("Signed in as {account}"),
                None => println!("Not signed in"),
            }
            Ok(())
        }
        Command::Dropbox { command: SignInCommand::Logout } => dropbox::logout(),
        Command::WebhookTest { name } => {
            let hooks: Vec<_> = cfg
                .connectors
                .webhooks
                .iter()
                .filter(|h| name.as_deref().is_none_or(|n| h.name == n || h.url == n))
                .collect();
            if hooks.is_empty() {
                bail!("no matching webhook in {}", Config::path()?.display());
            }
            let mut results = Vec::new();
            for hook in hooks {
                let result = webhook::send_test(hook).await;
                results.push(connectors::DeliveryResult {
                    connector: format!("webhook:{}", webhook::display_name(hook)),
                    ok: result.is_ok(),
                    detail: result.unwrap_or_else(|e| format!("{e:#}")),
                });
            }
            report(&results)
        }
        Command::Config => {
            let path = Config::path()?;
            println!("# {}", path.display());
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
    }
}

fn report(results: &[connectors::DeliveryResult]) -> Result<()> {
    for r in results {
        println!("{} {}: {}", if r.ok { "✓" } else { "✗" }, r.connector, r.detail);
    }
    if results.iter().any(|r| !r.ok) {
        bail!("some deliveries failed");
    }
    Ok(())
}

async fn record(cfg: Config, title: String, duration: Option<u64>) -> Result<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    let printer = tokio::spawn(print_events(rx));
    let recording = Recording::start(cfg, title, tx).await.context("starting recording")?;
    eprintln!("Recording \"{}\" — press Ctrl-C to stop.", recording.title);
    let limit = async {
        match duration {
            Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
            None => std::future::pending().await,
        }
    };
    let ended_itself = async {
        while !recording.is_finished() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = limit => {}
        _ = ended_itself => {}
    }
    eprintln!("\nStopping, finishing transcription…");
    let transcript = recording.stop().await?;
    printer.await?;
    println!("\nSaved transcript {} ({} segments)", transcript.id, transcript.segments.len());
    Ok(())
}

async fn print_events(mut rx: mpsc::UnboundedReceiver<SessionEvent>) {
    let mut partial_shown = false;
    while let Some(event) = rx.recv().await {
        if partial_shown && !matches!(event, SessionEvent::Partial { .. }) {
            eprint!("\r\x1b[2K");
            partial_shown = false;
        }
        match event {
            SessionEvent::Started { engine, .. } => eprintln!("Engine: {engine}"),
            SessionEvent::Partial { speaker, text } => {
                eprint!("\r\x1b[2K… {speaker:?}: {text}");
                let _ = std::io::stderr().flush();
                partial_shown = true;
            }
            SessionEvent::Segment { segment } => {
                println!("[{}] {:?}: {}", fmt_ts(segment.start_ms), segment.speaker, segment.text);
            }
            SessionEvent::Error { message } => eprintln!("! {message}"),
            SessionEvent::Finished { transcript_id } => eprintln!("Saved locally ({transcript_id})"),
            SessionEvent::Delivery { result } => {
                eprintln!("{} {}: {}", if result.ok { "✓" } else { "✗" }, result.connector, result.detail)
            }
            SessionEvent::Done { .. } | SessionEvent::Levels { .. } => {}
        }
    }
}
