//! Indexer: walk a tree, parse each file with tree-sitter, upsert the graph.

mod parser;
mod languages;

pub use parser::{ParsedSymbol, ParsedRelation, SymbolKind, RelationKind};

use anyhow::{Context, Result};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::Config;
use crate::db::Pool;

/// File extensions → language key understood by the parser.
fn detect_language(p: &Path) -> Option<&'static str> {
    match p.extension().and_then(|s| s.to_str())?.to_ascii_lowercase().as_str() {
        "rs"              => Some("rust"),
        "py" | "pyi"      => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx"      => Some("typescript"),
        _ => None,
    }
}

fn is_ignored(p: &Path) -> bool {
    // Skip common noise dirs. A full .gitignore parser is out of scope for v0.
    const IGN: &[&str] = &[
        ".git", "node_modules", "target", "dist", "build", ".venv", "venv",
        "__pycache__", ".next", ".cache", ".idea", ".vscode",
    ];
    p.components().any(|c| {
        let Some(s) = c.as_os_str().to_str() else { return false };
        IGN.contains(&s)
    })
}

pub fn index_tree(pool: &Pool, root: &Path, _cfg: &Config) -> Result<()> {
    tracing::info!(root = %root.display(), "full index");
    let mut n_files = 0usize;
    let mut n_syms  = 0usize;
    let mut n_rels  = 0usize;

    for entry in WalkDir::new(root).follow_links(false) {
        let entry = match entry { Ok(e) => e, Err(e) => { tracing::warn!(?e, "walk error"); continue; } };
        if !entry.file_type().is_file() { continue; }
        let path = entry.path();
        if is_ignored(path) { continue; }
        let Some(lang) = detect_language(path) else { continue; };

        match index_file(pool, root, path, lang) {
            Ok((s, r)) => { n_files += 1; n_syms += s; n_rels += r; }
            Err(e) => tracing::warn!(file = %path.display(), ?e, "index failure"),
        }
    }
    tracing::info!(n_files, n_syms, n_rels, "index done");
    Ok(())
}

/// Reindex a single file. Called both by the full walker and by the watcher.
pub fn index_file(pool: &Pool, root: &Path, path: &Path, lang: &str) -> Result<(usize, usize)> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let hash = {
        let mut h = Sha256::new(); h.update(content.as_bytes()); hex::encode(h.finalize())
    };
    let rel_path = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
    let line_count = content.lines().count() as i64;
    let size_bytes = content.len() as i64;

    // Parse first (before taking the DB lock).
    let parsed = parser::parse(lang, &rel_path, &content)
        .with_context(|| format!("parsing {rel_path}"))?;

    pool.with_conn_mut(|conn| {
        let tx = conn.transaction()?;

        // Upsert file row; delete previous symbols (CASCADE wipes relations & line_index).
        tx.execute(
            "INSERT INTO files (path, language, hash, line_count, size_bytes, last_indexed)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'))
             ON CONFLICT(path) DO UPDATE SET
               language=excluded.language,
               hash=excluded.hash,
               line_count=excluded.line_count,
               size_bytes=excluded.size_bytes,
               last_indexed=excluded.last_indexed",
            params![rel_path, lang, hash, line_count, size_bytes],
        )?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE path = ?1", params![rel_path], |r| r.get(0))?;
        tx.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
        tx.execute("DELETE FROM line_index WHERE file_id = ?1", params![file_id])?;

        // Insert symbols; track ids in a local map for parent linking.
        let mut local_ids: std::collections::HashMap<usize, i64> = Default::default();
        for (idx, s) in parsed.symbols.iter().enumerate() {
            let parent_id = s.parent_idx.and_then(|i| local_ids.get(&i).copied());
            tx.execute(
                "INSERT INTO symbols
                   (file_id, parent_symbol_id, name, qualified_name, kind, signature,
                    visibility, docstring, start_line, end_line, start_col, end_col, body_hash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    file_id, parent_id, s.name, s.qualified_name, s.kind.as_str(),
                    s.signature, s.visibility, s.docstring,
                    s.start_line, s.end_line, s.start_col, s.end_col,
                    s.body_hash.as_deref(),
                ],
            )?;
            local_ids.insert(idx, tx.last_insert_rowid());
        }

        // Insert relations. Try to resolve target_name -> target_symbol_id within same file.
        for r in &parsed.relations {
            let src_id = match local_ids.get(&r.source_idx) { Some(&v) => v, None => continue };
            let target_id: Option<i64> = if let Some(t) = r.target_name.as_deref() {
                tx.query_row(
                    "SELECT id FROM symbols WHERE qualified_name = ?1 OR name = ?1 LIMIT 1",
                    params![t], |row| row.get(0),
                ).ok()
            } else { None };
            tx.execute(
                "INSERT INTO relations
                   (source_symbol_id, target_symbol_id, target_name,
                    relation_kind, line, col, weight)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    src_id, target_id, r.target_name,
                    r.kind.as_str(), r.line, r.col, r.weight,
                ],
            )?;
        }

        // Rebuild line_index for this file.
        tx.execute(
            "INSERT OR REPLACE INTO line_index (file_id, line, symbol_id, relation_count)
             SELECT s.file_id, l.line, s.id,
                    (SELECT COUNT(*) FROM relations r WHERE r.source_symbol_id = s.id AND r.line = l.line)
             FROM symbols s
             JOIN (WITH RECURSIVE nums(line) AS
                     (SELECT 1 UNION ALL SELECT line+1 FROM nums WHERE line < ?1)
                   SELECT line FROM nums) l
               ON l.line BETWEEN s.start_line AND s.end_line
             WHERE s.file_id = ?2",
            params![line_count.max(1), file_id],
        )?;

        tx.commit()?;
        Ok((parsed.symbols.len(), parsed.relations.len()))
    })
}

/// Remove a file (and cascade its symbols/relations) from the DB.
pub fn remove_file(pool: &Pool, root: &Path, path: &Path) -> Result<()> {
    let rel_path = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
    pool.with_conn(|c| {
        c.execute("DELETE FROM files WHERE path = ?1", params![rel_path])?;
        Ok(())
    })
}

/// Helper for the watcher: pick any file path under root and reindex.
pub fn reindex_path(pool: &Pool, root: &Path, path: &PathBuf) -> Result<()> {
    if !path.exists() {
        return remove_file(pool, root, path);
    }
    if is_ignored(path) { return Ok(()); }
    let Some(lang) = detect_language(path) else { return Ok(()); };
    index_file(pool, root, path, lang).map(|_| ())
}
