//! Obsidian vault generator.
//!
//! Produces a folder layout:
//!
//!   /obsidian_vault/
//!     ├── Files/       one .md per indexed file, containing a TOC of its symbols
//!     ├── Classes/     one .md per class (or enum/trait)
//!     ├── Functions/   one .md per function/method
//!     ├── Variables/   one .md per variable/constant
//!     └── .obsidian/
//!           └── graph.json   (color groups for the Obsidian Graph View)
//!
//! Each note uses Wikilinks (`[[Other Symbol]]`) so the Graph View can draw
//! edges. The `.obsidian/graph.json` colours match the palette specified in
//! the project requirements.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::db::Pool;

const COLOR_FILE:     &str = "#999999";
const COLOR_CLASS:    &str = "#ff4d4d";
const COLOR_FUNCTION: &str = "#4d94ff";
const COLOR_VARIABLE: &str = "#ffdb4d";

pub fn generate(pool: &Pool, cfg: &Config) -> Result<()> {
    let vault = &cfg.vault;
    fs::create_dir_all(vault)?;
    for sub in ["Files", "Classes", "Functions", "Variables", ".obsidian"] {
        fs::create_dir_all(vault.join(sub))?;
    }
    write_graph_json(vault)?;

    pool.with_conn(|c| {
        // Files
        let mut stmt = c.prepare(
            "SELECT id, path, language, line_count FROM files ORDER BY path")?;
        let files: Vec<(i64, String, String, i64)> = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?.filter_map(Result::ok).collect();

        for (file_id, path, lang, lines) in &files {
            write_file_note(vault, c, *file_id, path, lang, *lines)?;
        }

        // Symbols (one per note depending on kind)
        let mut stmt = c.prepare(
            "SELECT id, name, qualified_name, kind, signature, start_line, end_line, file_id
             FROM symbols WHERE kind != 'file' ORDER BY kind, name")?;
        let rows: Vec<SymbolRow> = stmt.query_map([], |r| Ok(SymbolRow{
            id: r.get(0)?, name: r.get(1)?, qname: r.get(2)?, kind: r.get(3)?,
            signature: r.get(4)?, start: r.get(5)?, end: r.get(6)?, file_id: r.get(7)?,
        }))?.filter_map(Result::ok).collect();

        for s in &rows {
            write_symbol_note(vault, c, s)?;
        }

        Ok(())
    })?;

    tracing::info!(vault = %vault.display(), "obsidian vault generated");
    Ok(())
}

struct SymbolRow {
    id: i64, name: String, qname: String, kind: String,
    signature: Option<String>, start: i64, end: i64, file_id: i64,
}

fn subdir_for(kind: &str) -> &'static str {
    match kind {
        "class" | "enum" | "trait" | "module" => "Classes",
        "function" | "method"                 => "Functions",
        "variable" | "constant"               => "Variables",
        _ => "Files",
    }
}

fn sanitize(s: &str) -> String {
    s.replace(['/', '\\', ':', '<', '>', '|', '"', '*', '?'], "_")
}

fn write_graph_json(vault: &Path) -> Result<()> {
    #[derive(Serialize)]
    struct ColorGroup<'a> { query: &'a str, color: Color<'a> }
    #[derive(Serialize)]
    struct Color<'a> { a: f64, rgb: &'a str }
    #[derive(Serialize)]
    struct GraphJson<'a> {
        #[serde(rename="collapse-filter")]     collapse_filter: bool,
        #[serde(rename="search")]               search: &'a str,
        #[serde(rename="showTags")]             show_tags: bool,
        #[serde(rename="showAttachments")]      show_attachments: bool,
        #[serde(rename="hideUnresolved")]       hide_unresolved: bool,
        #[serde(rename="showOrphans")]          show_orphans: bool,
        #[serde(rename="collapse-color-groups")] collapse_color_groups: bool,
        #[serde(rename="colorGroups")]          color_groups: Vec<ColorGroup<'a>>,
        #[serde(rename="collapse-display")]     collapse_display: bool,
        #[serde(rename="showArrow")]            show_arrow: bool,
        #[serde(rename="textFadeMultiplier")]   text_fade: f64,
        #[serde(rename="nodeSizeMultiplier")]   node_size: f64,
        #[serde(rename="lineSizeMultiplier")]   line_size: f64,
        #[serde(rename="collapse-forces")]      collapse_forces: bool,
        #[serde(rename="centerStrength")]       center_strength: f64,
        #[serde(rename="repelStrength")]        repel_strength: f64,
        #[serde(rename="linkStrength")]         link_strength: f64,
        #[serde(rename="linkDistance")]         link_distance: f64,
        scale: f64,
        close: bool,
    }
    let g = GraphJson {
        collapse_filter: true, search: "", show_tags: false, show_attachments: false,
        hide_unresolved: false, show_orphans: true, collapse_color_groups: false,
        color_groups: vec![
            ColorGroup { query: "path:Files/",     color: Color { a: 1.0, rgb: COLOR_FILE     } },
            ColorGroup { query: "path:Classes/",   color: Color { a: 1.0, rgb: COLOR_CLASS    } },
            ColorGroup { query: "path:Functions/", color: Color { a: 1.0, rgb: COLOR_FUNCTION } },
            ColorGroup { query: "path:Variables/", color: Color { a: 1.0, rgb: COLOR_VARIABLE } },
        ],
        collapse_display: true, show_arrow: true,
        text_fade: 0.0, node_size: 1.0, line_size: 1.0,
        collapse_forces: true,
        center_strength: 0.5, repel_strength: 10.0, link_strength: 1.0, link_distance: 250.0,
        scale: 1.0, close: true,
    };
    let path = vault.join(".obsidian").join("graph.json");
    let mut f = fs::File::create(&path)?;
    f.write_all(serde_json::to_string_pretty(&g)?.as_bytes())?;
    Ok(())
}

fn write_file_note(
    vault: &Path, c: &rusqlite::Connection,
    file_id: i64, path: &str, lang: &str, lines: i64,
) -> Result<()> {
    let name = sanitize(path);
    let note = vault.join("Files").join(format!("{name}.md"));

    let mut contents = String::new();
    contents.push_str(&format!("---\ntype: file\nlanguage: {lang}\nlines: {lines}\n---\n\n"));
    contents.push_str(&format!("# `{path}`\n\n"));
    contents.push_str(&format!("**Language:** {lang} — **Lines:** {lines}\n\n"));
    contents.push_str("## Symbols\n\n");

    let mut stmt = c.prepare(
        "SELECT name, qualified_name, kind, start_line, end_line
         FROM symbols WHERE file_id = ?1 AND kind != 'file'
         ORDER BY start_line")?;
    let rows = stmt.query_map(params![file_id], |r| Ok((
        r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?,
        r.get::<_,i64>(3)?,    r.get::<_,i64>(4)?,
    )))?;
    for row in rows.flatten() {
        let (name, qname, kind, s, e) = row;
        contents.push_str(&format!(
            "- [[{qname}|{name}]] — `{kind}` (L{s}–L{e})\n",
            qname = qname, name = name, kind = kind, s = s, e = e
        ));
    }

    fs::write(note, contents)?;
    Ok(())
}

fn write_symbol_note(
    vault: &Path, c: &rusqlite::Connection, s: &SymbolRow,
) -> Result<()> {
    let dir = vault.join(subdir_for(&s.kind));
    let note = dir.join(format!("{}.md", sanitize(&s.qname)));
    let file_path: String = c.query_row(
        "SELECT path FROM files WHERE id = ?1", params![s.file_id], |r| r.get(0))?;

    let mut out = String::new();
    out.push_str(&format!(
        "---\ntype: {kind}\nfile: {file}\nlines: {a}-{b}\n---\n\n",
        kind=s.kind, file=file_path, a=s.start, b=s.end
    ));
    out.push_str(&format!("# {name}\n\n", name=s.name));
    if let Some(sig) = &s.signature {
        out.push_str(&format!("```\n{sig}\n```\n\n"));
    }
    out.push_str(&format!("**File:** [[{file_link}]]  \n**Kind:** `{kind}`  \n**Lines:** {a}–{b}\n\n",
        file_link = PathBuf::from("Files").join(sanitize(&file_path)).to_string_lossy(),
        kind=s.kind, a=s.start, b=s.end));

    // Outgoing edges
    let mut stmt = c.prepare(
        "SELECT relation_kind, COALESCE(target_name, ''), line
         FROM relations WHERE source_symbol_id = ?1 ORDER BY line")?;
    let out_rows: Vec<(String, String, i64)> = stmt.query_map(params![s.id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.filter_map(Result::ok).collect();
    if !out_rows.is_empty() {
        out.push_str("## Outgoing\n\n");
        for (kind, tgt, line) in &out_rows {
            if tgt.is_empty() { continue; }
            out.push_str(&format!("- `{kind}` [[{tgt}]] — L{line}\n"));
        }
        out.push('\n');
    }

    // Incoming edges (fan-in)
    let mut stmt = c.prepare(
        "SELECT s2.qualified_name, r.relation_kind, r.line
         FROM relations r JOIN symbols s2 ON s2.id = r.source_symbol_id
         WHERE r.target_symbol_id = ?1 ORDER BY r.line")?;
    let in_rows: Vec<(String, String, i64)> = stmt.query_map(params![s.id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.filter_map(Result::ok).collect();
    if !in_rows.is_empty() {
        out.push_str("## Incoming\n\n");
        for (q, kind, line) in &in_rows {
            out.push_str(&format!("- [[{q}]] — `{kind}` at L{line}\n"));
        }
        out.push('\n');
    }

    fs::write(note, out)?;
    Ok(())
}
