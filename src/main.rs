use anyhow::Result;
#[cfg(feature = "aws")]
use seiza_server::store_migration::{MigrationArgs, run as run_store_migration};
use seiza_server::{
    api::{AppState, router},
    config::Config,
    worker::{WorkerArgs, run as run_worker},
};
use tracing_subscriber::EnvFilter;

const STORE_MIGRATION_USAGE: &str = "usage: seiza-server migrate-store --from sqlx|dynamodb --to sqlx|dynamodb \\\n       [--sqlx-url URL] [--dynamodb-table TABLE] [--dry-run] [--resume]";
const SERVER_USAGE: &str = "usage: seiza-server [serve]
       seiza-server worker [--server http://api:8080] [--token TOKEN] [--mode http|sqs]
       seiza-server migrate-store --from sqlx|dynamodb --to sqlx|dynamodb \\
         [--sqlx-url URL] [--dynamodb-table TABLE] [--dry-run] [--resume]";

#[tokio::main]
async fn main() -> Result<()> {
    // reqwest is built with the `rustls-tls-webpki-roots-no-provider` variant
    // (see Cargo.toml), so a process-default rustls CryptoProvider must be
    // installed before any client is created. The AWS SDK installs one, but the
    // `worker` subcommand builds its reqwest client (WorkerClient::new) before it
    // loads aws-config, so without this the worker panics with "No provider set".
    // Install AWS-LC-RS up front so serve/worker/migrate-store are all safe
    // regardless of order (idempotent; a later install is a no-op).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "seiza_server=info,tower_http=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("worker") => run_worker(WorkerArgs::from_env_and_args(&args[1..])?).await,
        Some("migrate-store") => {
            if args[1..]
                .iter()
                .any(|argument| argument == "--help" || argument == "-h")
            {
                println!("{STORE_MIGRATION_USAGE}");
                return Ok(());
            }
            #[cfg(feature = "aws")]
            {
                run_store_migration(MigrationArgs::from_env_and_args(&args[1..])?).await
            }
            #[cfg(not(feature = "aws"))]
            {
                anyhow::bail!(
                    "migrate-store requires an AWS-enabled build; rerun with `cargo run --features aws -- migrate-store ...`"
                )
            }
        }
        Some("serve") | None => run_server().await,
        Some("--help") | Some("-h") => {
            println!("{SERVER_USAGE}");
            Ok(())
        }
        Some(command) => anyhow::bail!("unknown command `{command}`\n{SERVER_USAGE}"),
    }
}

async fn run_server() -> Result<()> {
    let config = Config::from_env()?;
    let bind_addr = config.bind_addr;
    let state = AppState::new(config).await?;
    state.start_background_tasks();
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(%bind_addr, "Seiza server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
