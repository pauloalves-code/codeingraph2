//! codeingraph2 — daemon entry point.
//!
//! Subcommands:
//!   daemon   : watcher + indexer + web UI
//!   index    : one-shot full index
//!   web      : only the web UI (useful in dev)
//!   health   : DB sanity check
//!   vault    : regenerate Obsidian vault
//!   claudemd : regenerate CLAUDE.md
//!   stats    : print graph stats

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use codeingraph2::{claudemd, config::Config, db, impact, indexer, obsidian, watcher, web};

#[derive(Parser, Debug)]
#[command(name = "codeingraph2", version, about = "Surgical context daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Daemon,
    Index { #[arg(long)] path: Option<PathBuf> },
    Web,
    Health,
    Vault,
    Claudemd,
    Stats,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let cfg = Config::from_env().context("loading config from env")?;
    tracing::info!(target = %cfg.target.display(), db = %cfg.db_path.display(), "codeingraph2 starting");

    let pool = db::open(&cfg)?;

    match cli.cmd {
        Cmd::Daemon => run_daemon(cfg, pool).await,
        Cmd::Index { path } => {
            let target = path.unwrap_or_else(|| cfg.target.clone());
            indexer::index_tree(&pool, &target, &cfg)?;
            impact::recompute(&pool)?;
            obsidian::generate(&pool, &cfg)?;
            claudemd::render(&pool, &cfg)?;
            Ok(())
        }
        Cmd::Web      => web::serve(cfg, pool).await,
        Cmd::Health   => { db::health(&pool)?; println!("ok"); Ok(()) }
        Cmd::Vault    => obsidian::generate(&pool, &cfg),
        Cmd::Claudemd => claudemd::render(&pool, &cfg),
        Cmd::Stats    => {
            let s = db::stats(&pool)?;
            println!("{}", serde_json::to_string_pretty(&s)?);
            Ok(())
        }
    }
}

/// Daemon: initial index → spawn web UI → run watcher (blocking).
/// tokio::select! returns as soon as either task errors or exits.
async fn run_daemon(cfg: Config, pool: db::Pool) -> Result<()> {
    tracing::info!("initial full index pass");
    indexer::index_tree(&pool, &cfg.target, &cfg)?;
    impact::recompute(&pool)?;
    obsidian::generate(&pool, &cfg)?;
    claudemd::render(&pool, &cfg)?;

    let web_cfg   = cfg.clone();
    let web_pool  = pool.clone();
    let watch_cfg = cfg.clone();
    let watch_pool = pool.clone();

    let web_task   = tokio::spawn(async move { web::serve(web_cfg, web_pool).await });
    let watch_task = tokio::task::spawn_blocking(move || watcher::run_blocking(watch_cfg, watch_pool));

    tokio::select! {
        r = web_task   => { r.context("web task join")?.context("web task error")?; }
        r = watch_task => { r.context("watcher task join")?.context("watcher task error")?; }
    }
    Ok(())
}
