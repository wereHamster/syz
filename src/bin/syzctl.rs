use clap::Parser;
use syz::tui::{run, Options};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// URL of the syzd server
    #[arg(short, long, env = "SYZD_URL", default_value = "http://127.0.0.1:7790")]
    url: String,

    /// Auth token
    #[arg(short, long, env = "SYZD_AUTH_TOKEN")]
    token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    run(Options {
        url: args.url,
        token: args.token,
    })
    .await?;

    Ok(())
}
