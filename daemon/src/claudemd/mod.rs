//! Render `CLAUDE.md` in the target tree from the template at
//! `$CODEINGRAPH2_TEMPLATES/CLAUDE.md.tmpl`.
//!
//! Idempotent: the block between `<!-- codeingraph2:begin -->` and
//! `<!-- codeingraph2:end -->` is replaced on every call, everything else in
//! the target file is preserved. So users can add project-specific
//! instructions *outside* that block without losing them.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use std::collections::BTreeMap;
use std::fs;

use crate::config::Config;
use crate::db::Pool;

const BEGIN: &str = "<!-- codeingraph2:begin -->";
const END:   &str = "<!-- codeingraph2:end -->";

pub fn render(pool: &Pool, cfg: &Config) -> Result<()> {
    let tmpl_path = cfg.templates_dir.join("CLAUDE.md.tmpl");
    let tmpl = fs::read_to_string(&tmpl_path)
        .with_context(|| format!("reading template {}", tmpl_path.display()))?;

    let ctx = gather(pool, cfg)?;
    let rendered = apply(&tmpl, &ctx);

    let dest = cfg.target.join("CLAUDE.md");
    let existing = fs::read_to_string(&dest).unwrap_or_default();

    let merged = if existing.contains(BEGIN) && existing.contains(END) {
        // Replace only the managed block, preserving user content.
        replace_block(&existing, &rendered)
    } else {
        rendered
    };

    fs::write(&dest, merged)
        .with_context(|| format!("writing {}", dest.display()))?;
    tracing::info!(path = %dest.display(), "CLAUDE.md rendered");
    Ok(())
}

fn replace_block(existing: &str, new_full: &str) -> String {
    let new_block = extract_block(new_full).unwrap_or(new_full.to_string());
    let before = existing.find(BEGIN).unwrap();
    let after  = existing.find(END).unwrap() + END.len();
    let mut out = String::new();
    out.push_str(&existing[..before]);
    out.push_str(&new_block);
    out.push_str(&existing[after..]);
    out
}

fn extract_block(full: &str) -> Option<String> {
    let b = full.find(BEGIN)?;
    let e = full.find(END)? + END.len();
    Some(full[b..e].to_string())
}

fn apply(tmpl: &str, vars: &BTreeMap<&'static str, String>) -> String {
    let mut out = tmpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

fn gather(pool: &Pool, cfg: &Config) -> Result<BTreeMap<&'static str, String>> {
    let mut v: BTreeMap<&'static str, String> = BTreeMap::new();

    let name = cfg.target.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
    v.insert("PROJECT_NAME", name);
    v.insert("GENERATED_AT", Utc::now().format("%Y-%m-%d %H:%M UTC").to_string());
    v.insert("TARGET_PATH", cfg.target.to_string_lossy().into());
    v.insert("DB_PATH",     cfg.db_path.to_string_lossy().into());
    v.insert("VAULT_PATH",  cfg.vault.to_string_lossy().into());

    pool.with_conn(|c| {
        let file_count:   i64 = c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let symbol_count: i64 = c.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let relation_count: i64 = c.query_row("SELECT COUNT(*) FROM relations",[], |r| r.get(0))?;
        v.insert("FILE_COUNT",     file_count.to_string());
        v.insert("SYMBOL_COUNT",   symbol_count.to_string());
        v.insert("RELATION_COUNT", relation_count.to_string());

        let mut langs = String::new();
        let mut stmt = c.prepare(
            "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY 2 DESC")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?)))? {
            let (l, n) = row?;
            if !langs.is_empty() { langs.push_str(", "); }
            langs.push_str(&format!("{l} ({n})"));
        }
        if langs.is_empty() { langs.push_str("—"); }
        v.insert("LANGUAGES", langs);

        v.insert("CONVENTIONS_BLOCK", detect_conventions(c)?);
        v.insert("TOP_FANIN_BLOCK",   top_list(c, "fan_in")?);
        v.insert("TOP_FANOUT_BLOCK",  top_list(c, "fan_out")?);
        Ok(())
    })?;

    Ok(v)
}

fn top_list(c: &rusqlite::Connection, col: &str) -> Result<String> {
    let sql = format!(
        "SELECT s.qualified_name, s.kind, f.path, i.{col}
         FROM impact_scores i
         JOIN symbols s ON s.id = i.symbol_id
         JOIN files   f ON f.id = s.file_id
         WHERE i.{col} > 0
         ORDER BY i.{col} DESC LIMIT 8"
    );
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map([], |r| Ok((
        r.get::<_,String>(0)?, r.get::<_,String>(1)?,
        r.get::<_,String>(2)?, r.get::<_,i64>(3)?,
    )))?;
    let mut out = String::new();
    let mut any = false;
    for row in rows.flatten() {
        any = true;
        let (q, k, f, n) = row;
        out.push_str(&format!("- `{q}` ({k}, {f}) — **{n}**\n"));
    }
    if !any { out.push_str("- (insuficientes dados — reindexe o projeto)\n"); }
    Ok(out)
}

fn detect_conventions(c: &rusqlite::Connection) -> Result<String> {
    // Very lightweight heuristics: what's the dominant casing for each kind?
    let kinds = ["function", "method", "class", "variable", "constant"];
    let mut lines = Vec::<String>::new();
    for k in kinds {
        let names: Vec<String> = {
            let mut stmt = c.prepare("SELECT name FROM symbols WHERE kind = ?1 LIMIT 200")?;
            stmt.query_map(params![k], |r| r.get::<_,String>(0))?
                .filter_map(Result::ok).collect()
        };
        if names.is_empty() { continue; }
        let style = dominant_style(&names);
        lines.push(format!("- **{k}s** tendem a `{style}`"));
    }
    if lines.is_empty() {
        Ok("- (convenções não detectadas ainda — reindexe com mais código)".into())
    } else {
        Ok(lines.join("\n"))
    }
}

fn dominant_style(names: &[String]) -> &'static str {
    let mut snake = 0; let mut camel = 0; let mut pascal = 0; let mut upper = 0;
    for n in names {
        if n.is_empty() { continue; }
        let c0 = n.chars().next().unwrap();
        if n.chars().all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit()) { upper += 1; }
        else if n.contains('_') { snake += 1; }
        else if c0.is_uppercase() { pascal += 1; }
        else { camel += 1; }
    }
    // pick max
    let m = [("snake_case", snake), ("camelCase", camel), ("PascalCase", pascal), ("UPPER_SNAKE", upper)];
    m.iter().max_by_key(|(_, n)| *n).map(|(s, _)| *s).unwrap_or("snake_case")
}
