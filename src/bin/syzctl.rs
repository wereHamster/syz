use clap::Parser;
use syz::tui::run;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    todo: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let _args = Args::parse();

    run()
}
