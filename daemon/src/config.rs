//! Runtime configuration loaded from env vars (set by docker-compose).

use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Source tree being indexed (bind-mounted at /target_code).
    pub target: PathBuf,
    /// Obsidian vault output dir (bind-mounted at /obsidian_vault).
    pub vault: PathBuf,
    /// SQLite DB path.
    pub db_path: PathBuf,
    /// Location of migrations / templates inside the image.
    pub templates_dir: PathBuf,
    pub migrations_dir: PathBuf,
    /// Milliseconds to coalesce rapid filesystem events.
    pub debounce_ms: u64,

    // --- Web UI ---
    pub web_enabled: bool,
    pub web_bind:    String,            // e.g. "0.0.0.0:7890"
    pub web_user:    Option<String>,
    /// Format: `$sha256$<salt_hex>$<hash_hex>` (written by install_global.sh)
    pub web_auth:    Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let getp = |k: &str, default: &str| -> PathBuf {
            std::env::var(k).map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(default))
        };
        Ok(Self {
            target:        getp("CODEINGRAPH2_TARGET",     "/target_code"),
            vault:         getp("CODEINGRAPH2_VAULT",      "/obsidian_vault"),
            db_path:       getp("CODEINGRAPH2_DB",         "/var/lib/codeingraph2/graph.db"),
            templates_dir: getp("CODEINGRAPH2_TEMPLATES",  "/opt/codeingraph2/templates"),
            migrations_dir:getp("CODEINGRAPH2_MIGRATIONS", "/opt/codeingraph2/migrations"),
            debounce_ms: std::env::var("CODEINGRAPH2_DEBOUNCE_MS")
                .ok().and_then(|s| s.parse().ok()).unwrap_or(750),

            web_enabled: std::env::var("WEB_ENABLED").map(|v| v != "0").unwrap_or(true),
            web_bind:    std::env::var("WEB_BIND").unwrap_or_else(|_| "0.0.0.0:7890".into()),
            web_user:    std::env::var("WEB_USER").ok().filter(|s| !s.is_empty()),
            web_auth:    std::env::var("WEB_AUTH").ok().filter(|s| !s.is_empty()),
        })
    }
}
