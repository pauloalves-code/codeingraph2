//! Filesystem watcher: coalesces inotify events and reindexes touched files.
//!
//! Blocking function — spawn it via `tokio::task::spawn_blocking` so it
//! doesn't hog an async worker.

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

    // Detect directories that are separately bind-mounted inside the target
    // host-path. In that case inotify reports their writes as paths under
    // cfg.target, so we must suppress them to prevent reindex loops.
    let vault_in_target = detect_dir_in_target(&cfg.target, &cfg.vault);
    if let Some(ref p) = vault_in_target {
        tracing::warn!(path = %p.display(),
            "vault is bind-mounted inside target; suppressing inotify events");
    }

    // The data directory (parent of db_path) can also end up inside the target
    // when PROJECT_DATA_DIR is a subdirectory of TARGET_CODE.
    let data_dir = cfg.db_path.parent().map(Path::to_path_buf);
    let data_in_target = data_dir.as_deref().and_then(|d| detect_dir_in_target(&cfg.target, d));
    if let Some(ref p) = data_in_target {
        tracing::warn!(path = %p.display(),
            "data dir is bind-mounted inside target; suppressing inotify events");
    }

    // Detect OTHER project data dirs that live as plain subdirectories inside
    // the target (e.g., when codeingraph2 manages multiple projects and all
    // their data dirs are under TARGET_CODE). We identify them by the presence
    // of graph.db + graph.db-shm (SQLite WAL mode marker) in direct children.
    let sibling_data_dirs = detect_sibling_data_dirs(&cfg.target, data_in_target.as_deref());
    for p in &sibling_data_dirs {
        tracing::warn!(path = %p.display(),
            "sibling data dir found inside target; suppressing inotify events");
    }

    let debounce = Duration::from_millis(cfg.debounce_ms);
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;

    tracing::info!("watcher active (debounce = {}ms)", cfg.debounce_ms);

    loop {
        let tick = Duration::from_millis(250);
        match rx.recv_timeout(tick) {
            Ok(Ok(ev)) => {
                if interesting(&ev) {
                    let before = pending.len();
                    for p in ev.paths {
                        if !is_daemon_generated(
                            &cfg, &p,
                            vault_in_target.as_deref(),
                            data_in_target.as_deref(),
                            &sibling_data_dirs,
                        ) {
                            pending.insert(p);
                        }
                    }
                    if pending.len() > before {
                        deadline = Some(Instant::now() + debounce);
                    }
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

/// Returns true for paths written by the daemon itself (must not trigger reindex).
fn is_daemon_generated(
    cfg: &Config,
    path: &Path,
    vault_in_target: Option<&Path>,
    data_in_target: Option<&Path>,
    sibling_data_dirs: &[PathBuf],
) -> bool {
    path == cfg.target.join("CLAUDE.md")
        || path.starts_with(&cfg.vault)
        || vault_in_target.map_or(false, |v| path.starts_with(v))
        || data_in_target.map_or(false, |d| path.starts_with(d))
        || sibling_data_dirs.iter().any(|d| path.starts_with(d))
}

/// Finds other codeingraph2 project data directories that live as plain
/// subdirectories inside the target. These are identified by the simultaneous
/// presence of `graph.db` and `graph.db-shm` (SQLite WAL-mode marker).
/// The `already_known` path (the current container's data dir) is excluded
/// from the results to avoid duplicates.
fn detect_sibling_data_dirs(target: &Path, already_known: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    collect_data_dirs(target, already_known, &mut dirs);
    // Also scan inside the optional "projects/" subdirectory where per-project
    // data lives when managed by install_global.sh.
    let projects = target.join("projects");
    if projects.is_dir() {
        collect_data_dirs(&projects, already_known, &mut dirs);
    }
    dirs
}

fn collect_data_dirs(dir: &Path, already_known: Option<&Path>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    out.extend(entries.filter_map(Result::ok).filter_map(|e| {
        let p = e.path();
        if already_known.map_or(false, |k| p == k) { return None; }
        if p.join("graph.db").exists() && p.join("graph.db-shm").exists() {
            Some(p)
        } else {
            None
        }
    }));
}

/// Detects whether `check_dir` is accessible as a direct child of `target` via
/// an overlapping bind-mount. When both directories are separately bind-mounted
/// into the container from host paths where one is a subdirectory of the other,
/// inotify reports events for `check_dir` under `target` rather than through
/// the `check_dir` mount. We detect this by comparing dev+ino of direct children
/// of target against the dev+ino of check_dir itself.
#[cfg(unix)]
fn detect_dir_in_target(target: &Path, check_dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let cm = std::fs::metadata(check_dir).ok()?;
    let check_id = (cm.dev(), cm.ino());
    std::fs::read_dir(target).ok()?.filter_map(Result::ok).find(|e| {
        e.metadata().ok().map_or(false, |m| (m.dev(), m.ino()) == check_id)
    }).map(|e| e.path())
}

#[cfg(not(unix))]
fn detect_dir_in_target(_target: &Path, _check_dir: &Path) -> Option<PathBuf> {
    None
}
