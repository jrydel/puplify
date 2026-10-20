use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use puplify_core::{
    audio,
    calendar,
    config::{Config, ExportFormat},
    connectors::{self, dropbox, google, microsoft, webhook},
    session::{self, Recording, SessionEvent},
    store,
    stt::models,
    transcript::fmt_ts,
};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "puplify", version, about = "Meeting transcription with calendar auto-recording and connectors (OneDrive, Google Drive, Dropbox, Notion, webhooks, folders)")]
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
    /// Connect or disconnect the Microsoft account (OneDrive export, Outlook calendar).
    Microsoft {
        #[command(subcommand)]
        command: SignInCommand,
    },
    /// Connect or disconnect the Google account (Google Drive export, Google Calendar).
    Google {
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
    /// List upcoming online meetings from the enabled calendars.
    Calendar {
        /// How many hours ahead to look.
        #[arg(long, default_value_t = 24)]
        hours: u32,
    },
    /// Show whether another application is using the microphone.
    MicActivity,
    /// Print the config file path and contents.
    Config,
}

#[derive(Subcommand)]
enum ModelsCommand {
    List,
    Download { name: String },
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
        Command::Microsoft { command: SignInCommand::Login } => {
            let account = microsoft::login(announce_and_open).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.onedrive.enabled && !cfg.calendar.microsoft {
                println!(
                    "Note: nothing uses this account yet; set connectors.onedrive.enabled and/or calendar.microsoft = true in {}",
                    Config::path()?.display()
                );
            }
            Ok(())
        }
        Command::Microsoft { command: SignInCommand::Status } => print_status(microsoft::status()?),
        Command::Microsoft { command: SignInCommand::Logout } => microsoft::logout(),
        Command::Google { command: SignInCommand::Login } => {
            let account = google::login(announce_and_open).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.google_drive.enabled && !cfg.calendar.google {
                println!(
                    "Note: nothing uses this account yet; set connectors.google_drive.enabled and/or calendar.google = true in {}",
                    Config::path()?.display()
                );
            }
            Ok(())
        }
        Command::Google { command: SignInCommand::Status } => print_status(google::status()?),
        Command::Google { command: SignInCommand::Logout } => google::logout(),
        Command::Dropbox { command: SignInCommand::Login } => {
            let account = dropbox::login(announce_and_open).await?;
            println!("Signed in as {account}");
            if !cfg.connectors.dropbox.enabled {
                println!("Note: Dropbox export is disabled; set connectors.dropbox.enabled = true in {}", Config::path()?.display());
            }
            Ok(())
        }
        Command::Dropbox { command: SignInCommand::Status } => print_status(dropbox::status()?),
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
        Command::Calendar { hours } => {
            let (meetings, warnings) = calendar::upcoming(&cfg, chrono::Duration::hours(hours.into())).await;
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if meetings.is_empty() && warnings.is_empty() {
                println!("No online meetings found. Enable a calendar under calendar.google / calendar.microsoft in {}", Config::path()?.display());
            }
            for m in &meetings {
                let local = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string();
                println!(
                    "{} - {}  {:<9}  {}  {}",
                    local(m.start),
                    &local(m.end)[11..],
                    format!("{:?}", m.provider).to_lowercase(),
                    m.title,
                    m.join_url.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        Command::MicActivity => {
            println!("{}", serde_json::to_string_pretty(&audio::activity::mic_activity()?)?);
            Ok(())
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

fn print_status(account: Option<String>) -> Result<()> {
    match account {
        Some(account) => println!("Signed in as {account}"),
        None => println!("Not signed in"),
    }
    Ok(())
}

/// Sign-in `open_url` callback: tell the user where to go and open the browser.
fn announce_and_open(url: &str) {
    println!("Opening your browser… If it doesn't open, visit: {url}");
    open_browser(url);
}

/// Best effort: the sign-in URL is also printed, so failures are ignored.
fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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
