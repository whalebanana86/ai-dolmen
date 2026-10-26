use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dolmen_core::Error;
use dolmen_core::postgres::{ConnectConfig, Session};
use dolmen_core::snapshot::{ChangeKind, FileChange, MARKER, Snapshot};

/// Dolmen command-line tool.
#[derive(Parser)]
#[command(name = "dolmen", version = dolmen_core::VERSION, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Schema snapshots: one DDL file per object, kept in Git.
    #[command(subcommand)]
    Schema(SchemaCommand),
}

#[derive(Subcommand)]
enum SchemaCommand {
    /// Write the database's schema snapshot into a folder.
    Dump(SnapshotArgs),
    /// Compare the database with a snapshot folder.
    ///
    /// Exits with 0 when they match, 1 when the database differs (drift), and
    /// 2 on errors, so it can fail a CI job.
    Check(SnapshotArgs),
}

#[derive(clap::Args)]
struct SnapshotArgs {
    /// Connection string, e.g. postgres://user@host:5432/db?sslmode=require.
    /// Without a password in it, PGPASSWORD is used.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    url: String,
    /// The snapshot folder.
    #[arg(long)]
    dir: PathBuf,
}

#[tokio::main]
async fn main() -> ExitCode {
    let Command::Schema(command) = Cli::parse().command;
    let result = match command {
        SchemaCommand::Dump(args) => dump(&args).await,
        SchemaCommand::Check(args) => check(&args).await,
    };
    result.unwrap_or_else(|err| {
        eprintln!("error: {err}");
        ExitCode::from(2)
    })
}

async fn dump(args: &SnapshotArgs) -> Result<ExitCode, Error> {
    let (current, saved) = snapshots(args).await?;
    let changes = saved.changes(&current);
    current.write(&args.dir)?;
    if changes.is_empty() {
        println!("{} already matches the database", args.dir.display());
    } else {
        println!(
            "Updated {} in {}:",
            files(changes.len()),
            args.dir.display()
        );
        print_list(&changes);
    }
    Ok(ExitCode::SUCCESS)
}

async fn check(args: &SnapshotArgs) -> Result<ExitCode, Error> {
    // Without the marker every object would show as added, which hides a wrong path.
    if !args.dir.join(MARKER).exists() {
        return Err(Error::Storage {
            message: format!(
                "{} holds no schema snapshot; write one with `dolmen schema dump` first",
                args.dir.display()
            ),
        });
    }
    let (current, saved) = snapshots(args).await?;
    // From the snapshot to the database: what the database has that the snapshot doesn't.
    let changes = saved.changes(&current);
    if changes.is_empty() {
        println!(
            "{} matches the database ({})",
            args.dir.display(),
            files(current.files().len())
        );
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "The database differs from {} in {}:",
        args.dir.display(),
        files(changes.len())
    );
    print_list(&changes);
    for change in &changes {
        println!();
        print!("{}", change.diff);
    }
    Ok(ExitCode::from(1))
}

/// The database's current snapshot and the one saved in the folder.
async fn snapshots(args: &SnapshotArgs) -> Result<(Snapshot, Snapshot), Error> {
    let session = Session::connect(&ConnectConfig::parse(&args.url)?).await?;
    Ok((
        Snapshot::of(&session.introspect().await?),
        Snapshot::read(&args.dir)?,
    ))
}

fn print_list(changes: &[FileChange]) {
    for change in changes {
        let letter = match change.kind {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Removed => 'D',
        };
        println!("  {letter} {}", change.path);
    }
}

fn files(count: usize) -> String {
    format!("{count} {}", if count == 1 { "file" } else { "files" })
}
