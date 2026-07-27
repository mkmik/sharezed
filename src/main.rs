use clap::{Parser, Subcommand};

/// direnv for everything a shell knows.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Capture the bootstrap script's shell state and publish a delta.
    Reload,
    /// Show cursor vs head, pending entries, conflicts.
    Status,
    /// Print the shell integration snippet; eval it in .zshrc.
    Hook { shell: String },
}

fn main() {
    // ponytail: stubs only. See docs/PRD.md §6 for the full surface.
    match Cli::parse().command {
        Command::Reload => todo!("capture + publish"),
        Command::Status => todo!("cursor vs head"),
        Command::Hook { shell } => todo!("emit {shell} integration"),
    }
}

#[test]
fn cli_is_valid() {
    use clap::CommandFactory;
    Cli::command().debug_assert();
}
