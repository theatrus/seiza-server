use anyhow::Result;
use seiza_server::{
    api::{AppState, router},
    config::Config,
    worker::{WorkerArgs, run as run_worker, worker_usage},
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "seiza_server=info,tower_http=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("worker") => run_worker(WorkerArgs::from_env_and_args(&args[1..])?).await,
        Some("serve") | None => run_server().await,
        Some("--help") | Some("-h") => {
            println!("usage: seiza-server [serve]\n       {}", worker_usage());
            Ok(())
        }
        Some(command) => anyhow::bail!(
            "unknown command `{command}`\nusage: seiza-server [serve]\n       {}",
            worker_usage()
        ),
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
