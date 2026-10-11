use anyhow::Context;
use clap::{Parser, Subcommand};
use syz::core::platform::Platform;
use syz::tui::{run, Options};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// URL of the syzd server
    #[arg(short, long, env = "SYZD_URL", default_value = "http://127.0.0.1:7790")]
    url: String,

    /// Auth token [default: contents of /var/lib/syz/auth-token]
    #[arg(short, long, env = "SYZD_AUTH_TOKEN")]
    token: Option<String>,

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let token = match args.token {
        Some(token) => token,
        None => syz::auth_token::read(std::path::Path::new(syz::auth_token::CLIENT_PATH))
            .context("No auth token given, set SYZD_AUTH_TOKEN or use --token")?,
    };

    match args.command {
        Some(Commands::Add { argument }) => {
            let parts: Vec<&str> = argument.split(':').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid argument format. Expected platform:repository");
            }

            let platform: Platform = parts[0].parse()?;
            let repository = parts[1].to_string();

            let client = reqwest::Client::new();
            let payload = serde_json::json!({
                "type": "AddProject",
                "platform": platform,
                "repository": repository,
            });

            client
                .post(format!("{}/messages", args.url))
                .header("Authorization", format!("Bearer {}", token))
                .json(&payload)
                .send()
                .await?;

            println!("Project added successfully (command sent).");
        }
        Some(Commands::Remove { argument }) => {
            let parts: Vec<&str> = argument.split(':').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid argument format. Expected platform:repository");
            }

            let platform: Platform = parts[0].parse()?;
            let repository = parts[1].to_string();

            let client = reqwest::Client::new();
            let payload = serde_json::json!({
                "type": "RemoveProject",
                "platform": platform,
                "repository": repository,
            });

            client
                .post(format!("{}/messages", args.url))
                .header("Authorization", format!("Bearer {}", token))
                .json(&payload)
                .send()
                .await?;

            println!("Project removal command sent.");
        }
        None => {
            run(Options {
                url: args.url,
                token,
            })
            .await?;
        }
    }

    Ok(())
}
