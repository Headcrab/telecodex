mod app;
mod codex;
mod codex_history;
mod commands;
mod config;
mod limits;
mod models;
mod render;
mod store;
mod telegram;
mod transcribe;
mod user_input;

use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--probe-native") {
        if !(args.len() == 4 || (args.len() == 5 && args[4] == "--create-thread-probes")) {
            anyhow::bail!(
                "Usage: telecodex --probe-native CODEX_BINARY CWD [--create-thread-probes]"
            );
        }
        let cwd = std::fs::canonicalize(&args[3])?;
        if !cwd.is_dir() {
            anyhow::bail!("native probe cwd must be a directory");
        }
        let result =
            codex::probe_native(std::path::Path::new(&args[2]), &cwd, args.len() == 5).await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("telecodex=info,reqwest=warn")),
        )
        .with_target(false)
        .compact()
        .init();

    if let Some(delay_ms) = restart_delay_ms_from_env() {
        sleep(Duration::from_millis(delay_ms)).await;
    }

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "telecodex.toml".to_string());
    let config = config::Config::load(config_path.into())?;
    let app = app::App::bootstrap(config).await?;
    app.run().await
}

fn restart_delay_ms_from_env() -> Option<u64> {
    std::env::var("TELECODEX_RESTART_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
}
