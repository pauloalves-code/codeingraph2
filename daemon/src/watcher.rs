//! Filesystem watcher: coalesces inotify events and reindexes touched files.
//!
//! Blocking function — spawn it via `tokio::task::spawn_blocking` so it
//! doesn't hog an async worker.

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use crate::claudemd;
use crate::config::Config;
use crate::db::Pool;
use crate::impact;
use crate::indexer;
use crate::obsidian;

pub fn run_blocking(cfg: Config, pool: Pool) -> Result<()> {
    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx)?;
    watcher.watch(&cfg.target, RecursiveMode::Recursive)?;

    let debounce = Duration::from_millis(cfg.debounce_ms);
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;

    tracing::info!("watcher active (debounce = {}ms)", cfg.debounce_ms);

    loop {
        let tick = Duration::from_millis(250);
        match rx.recv_timeout(tick) {
            Ok(Ok(ev)) => {
                if interesting(&ev) {
                    for p in ev.paths { pending.insert(p); }
                    deadline = Some(Instant::now() + debounce);
                }
            }
            Ok(Err(e)) => tracing::warn!(?e, "watcher error"),
            Err(_) => {}
        }

        if let Some(d) = deadline {
            if Instant::now() >= d && !pending.is_empty() {
                let batch: Vec<_> = pending.drain().collect();
                deadline = None;
                tracing::info!(n = batch.len(), "reindex batch");
                for p in &batch {
                    if let Err(e) = indexer::reindex_path(&pool, &cfg.target, p) {
                        tracing::warn!(path = %p.display(), ?e, "reindex failed");
                    }
                }
                let _ = impact::recompute(&pool);
                if cfg.vault_enabled { let _ = obsidian::generate(&pool, &cfg); }
                let _ = claudemd::render(&pool, &cfg);
            }
        }
    }
}

fn interesting(ev: &Event) -> bool {
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}
