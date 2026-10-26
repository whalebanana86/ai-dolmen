use clap::Parser;

/// Dolmen command-line tool.
#[derive(Parser)]
#[command(name = "dolmen", version = dolmen_core::VERSION, about, arg_required_else_help = true)]
struct Cli {}

fn main() {
    Cli::parse();
}
