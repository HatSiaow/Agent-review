use anyhow::Context as _;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rr-agent")]
#[command(about = "Neighbourhood Restaurant Review Agent CLI")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Database migrations.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },

    /// Start Google OAuth flow (placeholder).
    GoogleAuth,
}

#[derive(Debug, Subcommand)]
enum MigrateCommand {
    Up,
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;
    let args = Args::parse();

    match args.command {
        Command::Migrate { command } => {
            let Some(cfg) = storage::PgRepositoryConfig::from_env() else {
                anyhow::bail!("DATABASE_URL must be set");
            };
            let repo = storage::PgRepository::connect(&cfg).await.context("connect db")?;
            match command {
                MigrateCommand::Up => {
                    repo.migrate().context("migrate")?;
                    tracing::info!("migrations complete");
                }
                MigrateCommand::Status => {
                    tracing::info!("migrations status is not implemented yet");
                }
            }
        }
        Command::GoogleAuth => {
            tracing::info!("google-auth flow is not implemented yet");
        }
    }

    Ok(())
}

