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
                    repo.migrate().await.context("migrate")?;
                    tracing::info!("migrations complete");
                }
                MigrateCommand::Status => {
                    let statuses = repo
                        .migration_status()
                        .await
                        .context("query migration status")?;

                    println!("Migration status:");
                    println!("{:<10} {:<50} {}", "Version", "Description", "Status");
                    println!("{}", "-".repeat(72));
                    for s in &statuses {
                        let label = if s.applied { "Applied" } else { "Pending" };
                        println!("{:<10} {:<50} {}", s.version, s.description, label);
                    }

                    let all_applied = statuses.iter().all(|s| s.applied);
                    if all_applied {
                        println!("\nAll migrations are up to date.");
                    } else {
                        println!("\nSome migrations are pending. Run 'migrate up' to apply them.");
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::GoogleAuth => {
            tracing::info!("google-auth flow is not implemented yet");
        }
    }

    Ok(())
}

