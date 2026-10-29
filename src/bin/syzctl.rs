use std::path::PathBuf;

use anyhow::Context;
use clap::{ArgGroup, Parser, Subcommand};
use syz::core::application::Application;
use syz::core::database::pk;
use syz::core::message::Payload;
use syz::core::platform::Platform;
use syz::tui::{run, Connection};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(group(
    ArgGroup::new("local_mode")
        .args(["local", "data_dir"])
        .multiple(true)
        .conflicts_with_all(["url", "token"]),
))]
struct Args {
    /// URL of the syzd server
    #[arg(short, long, env = "SYZD_URL", default_value = "http://127.0.0.1:7790")]
    url: String,

    /// Auth token [default: contents of /var/lib/syz/auth-token]
    #[arg(short, long, env = "SYZD_AUTH_TOKEN")]
    token: Option<String>,

    /// Run the application in this process instead of connecting to a syzd server
    #[arg(long)]
    local: bool,

    /// Directory where the local application keeps its database. Implies --local
    /// [default: syz in the user's data directory, ~/.local/share/syz on Linux,
    /// ~/Library/Application Support/syz on macOS]
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Directory for everything the application needs only temporarily, as for syzd
    #[arg(long, requires = "local_mode", default_value_os_t = std::env::temp_dir())]
    work_dir: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Add a new project in the format platform:repository
    Add {
        /// The project to add (e.g., github:owner/repo)
        argument: String,
    },
    /// Remove a project in the format platform:repository
    Remove {
        /// The project to remove (e.g., github:owner/repo)
        argument: String,
    },
}

impl Commands {
    fn payload(&self) -> anyhow::Result<Payload> {
        let (Commands::Add { argument } | Commands::Remove { argument }) = self;

        let parts: Vec<&str> = argument.split(':').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid argument format. Expected platform:repository");
        }

        let platform: Platform = parts[0].parse()?;
        let repository = parts[1].to_string();

        Ok(match self {
            Commands::Add { .. } => Payload::AddProject {
                platform,
                repository,
            },
            Commands::Remove { .. } => Payload::RemoveProject {
                platform,
                repository,
            },
        })
    }

    fn done(&self) -> &'static str {
        match self {
            Commands::Add { .. } => "Project added successfully (command sent).",
            Commands::Remove { .. } => "Project removal command sent.",
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.local || args.data_dir.is_some() {
        let data_dir = match args.data_dir {
            Some(data_dir) => data_dir,
            None => default_data_dir()?,
        };
        run_local(data_dir, args.work_dir, args.command).await
    } else {
        run_remote(args.url, args.token, args.command).await
    }
}

/// `syz` in the user's data directory: `$XDG_DATA_HOME` (or `~/.local/share`) on Linux,
/// `~/Library/Application Support` on macOS.
fn default_data_dir() -> anyhow::Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("Could not determine the user's data directory, use --data-dir")?;
    Ok(dir.join("syz"))
}

/// Talk to a syzd server.
async fn run_remote(
    url: String,
    token: Option<String>,
    command: Option<Commands>,
) -> anyhow::Result<()> {
    let token = match token {
        Some(token) => token,
        None => syz::auth_token::read(std::path::Path::new(syz::auth_token::CLIENT_PATH))
            .context("No auth token given, set SYZD_AUTH_TOKEN or use --token")?,
    };

    match command {
        Some(command) => {
            let payload = command.payload()?;

            reqwest::Client::new()
                .post(format!("{}/messages", url))
                .header("Authorization", format!("Bearer {}", token))
                .json(&payload)
                .send()
                .await?;

            println!("{}", command.done());
        }
        None => run(Connection::Remote { url, token }).await?,
    }

    Ok(())
}

/// Run the application in this process.
async fn run_local(
    data_dir: PathBuf,
    work_dir: PathBuf,
    command: Option<Commands>,
) -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Before the application is built, so that a bad argument is reported first.
    let payload = command.as_ref().map(Commands::payload).transpose()?;

    let application = Application::new(&data_dir, work_dir).await?;

    match (command, payload) {
        (Some(command), Some(payload)) => {
            // Only here for the one message, so no audit. The jobs the message asks for, such
            // as closing the pull requests of a removed project, are waited for.
            let application = application.with_audit_schedule(false);
            let dispatcher = application.dispatcher();
            let handle = application.start();

            handle.send_and_wait(pk(), payload).await?;
            dispatcher.idle().await;

            println!("{}", command.done());
        }
        // The TUI owns the terminal, its log panel shows what the application logs.
        _ => {
            run(Connection::Local(
                application.with_stdout_logging(false).start(),
            ))
            .await?
        }
    }

    Ok(())
}
