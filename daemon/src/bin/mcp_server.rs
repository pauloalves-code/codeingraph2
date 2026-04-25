//! MCP server (stdio transport).
//!
//! Reads newline-delimited JSON-RPC 2.0 requests on stdin and writes responses
//! on stdout. Protocol subset implemented:
//!
//!   * initialize
//!   * tools/list
//!   * tools/call              — with the tool set below
//!
//! Tools:
//!   * `list_projects`          list registered projects (when using global registry)
//!   * `get_surgical_context`   surgical slices with full source — no Read needed
//!   * `patch_symbol`           edit a symbol by name — no old_string needed
//!   * `query_graph`            search by name / kind / file
//!   * `get_symbol`             full metadata for one symbol
//!   * `get_callers`            who calls symbol X
//!   * `get_callees`            what does symbol X call
//!   * `graph_stats`            counts
//!
//! All tools accept an optional `"project"` parameter when a CODEINGRAPH2_REGISTRY
//! env var points to a registry.json. Without it the first registered project is used.
//!
//! The server opens the SQLite DB in read-only mode; the daemon owns writes.
//! patch_symbol writes directly to the target files — the daemon reindexes automatically.

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// --- registry ----------------------------------------------------------------

#[derive(Deserialize, Clone)]
struct ProjectEntry {
    target: PathBuf,
    db: PathBuf,
}

#[derive(Deserialize)]
struct Registry {
    projects: HashMap<String, ProjectEntry>,
}

fn load_registry() -> Option<Registry> {
    let path = std::env::var("CODEINGRAPH2_REGISTRY").ok()?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn resolve_context(registry: Option<&Registry>, args: &Value) -> Result<(PathBuf, PathBuf)> {
    if let Some(reg) = registry {
        let project_name = args.get("project").and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| std::env::var("CODEINGRAPH2_PROJECT").ok());

        let entry = if let Some(ref name) = project_name {
            reg.projects.get(name)
                .ok_or_else(|| anyhow!("project not found: {name}. Use list_projects to see available projects."))?
        } else {
            reg.projects.values().next()
                .ok_or_else(|| anyhow!("registry is empty"))?
        };
        return Ok((entry.db.clone(), entry.target.clone()));
    }

    let db: PathBuf = std::env::var("CODEINGRAPH2_DB")
        .unwrap_or_else(|_| "/var/lib/codeingraph2/graph.db".into())
        .into();
    let target: PathBuf = std::env::var("CODEINGRAPH2_TARGET")
        .unwrap_or_else(|_| "/target_code".into())
        .into();
    Ok((db, target))
}

// --- main --------------------------------------------------------------------

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();

    let registry = load_registry();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(e) => { tracing::error!(?e, "stdin"); break; } };
        if line.trim().is_empty() { continue; }

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(out, "{}", error_response(Value::Null, -32700, &format!("parse error: {e}")));
                continue;
            }
        };

        let resp = handle(registry.as_ref(), &req);
        let resp_line = match resp {
            Ok(result) => ok_response(req.id.clone(), result),
            Err(e)     => error_response(req.id.clone(), -32000, &e.to_string()),
        };
        writeln!(out, "{resp_line}")?;
        out.flush()?;
    }
    Ok(())
}

#[derive(Deserialize, Debug)]
struct Request {
    #[serde(default)] jsonrpc: String,
    #[serde(default)] id: Value,
    method: String,
    #[serde(default)] params: Value,
}

#[derive(Serialize)]
struct Response<'a> {
    jsonrpc: &'a str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")] result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")] error:  Option<ErrObj<'a>>,
}

#[derive(Serialize)]
struct ErrObj<'a> { code: i64, message: &'a str }

fn ok_response(id: Value, result: Value) -> String {
    serde_json::to_string(&Response { jsonrpc: "2.0", id, result: Some(result), error: None }).unwrap()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    serde_json::to_string(&Response { jsonrpc: "2.0", id, result: None, error: Some(ErrObj { code, message }) }).unwrap()
}

fn handle(registry: Option<&Registry>, req: &Request) -> Result<Value> {
    if req.jsonrpc != "2.0" && !req.jsonrpc.is_empty() {
        return Err(anyhow!("unsupported jsonrpc version"));
    }
    match req.method.as_str() {
        "initialize"  => Ok(initialize()),
        "tools/list"  => Ok(tools_list()),
        "tools/call"  => tools_call(registry, &req.params),
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
        other => Err(anyhow!("method not found: {other}")),
    }
}

fn initialize() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "codeingraph2",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": concat!(
            "# CodeInGraph2 — Token-efficient code navigation\n\n",
            "## Multi-project support:\n",
            "- Use `list_projects` to see available projects.\n",
            "- Pass `\"project\": \"<name>\"` to any tool to target a specific project.\n",
            "- Omit `project` to use the default (first registered) project.\n\n",
            "## Core workflow (zero extra Reads):\n",
            "1. `get_surgical_context` — returns file + exact lines + **full source** for every impacted snippet.\n",
            "2. `patch_symbol`         — edit a symbol by name. No Read, no old_string needed.\n\n",
            "## Other tools:\n",
            "- `query_graph name=X`    — find any symbol instantly (replaces grep).\n",
            "- `get_symbol name`       — signature + location for one symbol.\n",
            "- `get_callers X depth=1` — blast radius before changing a signature.\n",
            "- `graph_stats`           — explore an unknown codebase structure first.\n\n",
            "## Hard rules:\n",
            "- NEVER Read a file to find a symbol — use query_graph.\n",
            "- NEVER Read after get_surgical_context — source is already in the response.\n",
            "- If you must Read, always use offset=start_line-1 and limit=end_line-start_line+1.\n",
            "- NEVER use Edit with old_string for a located symbol — use patch_symbol instead."
        )
    })
}

fn project_param() -> Value {
    json!({
        "type": "string",
        "description": "Project name (from list_projects). Omit to use the default project."
    })
}

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "list_projects",
            "description": "List all projects registered in the global registry. Use to discover project names for the `project` parameter.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_surgical_context",
            "description": "TOKEN SAVER: Returns code snippets impacted by a symbol — each snippet includes its full `source` so no Read is needed afterward. Use before any refactor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Qualified name (e.g. 'MyModule::my_fn'), plain name, or 'path/to/file.rs:42' for line-based lookup."
                    },
                    "depth":        { "type": "integer", "minimum": 1, "maximum": 5, "default": 1, "description": "1 = direct callers/callees only (fastest). 2 = transitive." },
                    "max_snippets": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 },
                    "project":      project_param()
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "patch_symbol",
            "description": "TOKEN SAVER: Edit a symbol by name — looks up its exact file:lines in the graph and replaces them. No Read or old_string needed. The daemon reindexes automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol":     { "type": "string", "description": "Qualified name or plain name of the symbol to replace." },
                    "new_source": { "type": "string", "description": "Complete new source code for this symbol (replaces the current start_line..end_line block)." },
                    "project":    project_param()
                },
                "required": ["symbol", "new_source"]
            }
        },
        {
            "name": "query_graph",
            "description": "TOKEN SAVER: Find symbols by name/kind/file — returns file + exact line numbers. Use INSTEAD of grep.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name":    { "type": "string", "description": "Substring match against symbol name and qualified name." },
                    "kind":    { "type": "string", "enum": ["file","class","function","method","variable","constant","enum","trait","module"] },
                    "file":    { "type": "string", "description": "File path prefix filter (e.g. 'src/web')." },
                    "limit":   { "type": "integer", "default": 50, "description": "Max results." },
                    "project": project_param()
                }
            }
        },
        {
            "name": "get_symbol",
            "description": "Full metadata for one symbol: signature, file, exact lines, visibility, docstring.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol":  { "type": "string", "description": "Qualified name or plain name." },
                    "project": project_param()
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "get_callers",
            "description": "Who calls symbol X? Returns transitive callers up to depth N with file:line. Use to understand blast radius before changing a function signature.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol":  { "type": "string" },
                    "depth":   { "type": "integer", "default": 2, "maximum": 5 },
                    "project": project_param()
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "get_callees",
            "description": "What does symbol X call? Returns outbound edges with file:line.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol":  { "type": "string" },
                    "project": project_param()
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "graph_stats",
            "description": "Global graph counts (files, symbols, relations) and per-language breakdown. Use first when exploring an unknown codebase.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_param()
                }
            }
        }
    ]})
}

fn tools_call(registry: Option<&Registry>, params: &Value) -> Result<Value> {
    let name = params.get("name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call: missing 'name'"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    if name == "list_projects" {
        return list_projects(registry);
    }

    let (db_path, target) = resolve_context(registry, &args)?;
    let conn = codeingraph2::db::open_readonly(&db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;

    let payload = match name {
        "get_surgical_context" => get_surgical_context(&conn, &args, &target)?,
        "patch_symbol"         => patch_symbol(&conn, &args, &target)?,
        "query_graph"          => query_graph(&conn, &args)?,
        "get_symbol"           => get_symbol(&conn, &args)?,
        "get_callers"          => get_callers(&conn, &args)?,
        "get_callees"          => get_callees(&conn, &args)?,
        "graph_stats"          => graph_stats(&conn)?,
        other => return Err(anyhow!("unknown tool: {other}")),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload)? }],
        "isError": false,
    }))
}

// --- helpers -----------------------------------------------------------------

fn list_projects(registry: Option<&Registry>) -> Result<Value> {
    let projects: Vec<Value> = registry
        .map(|reg| {
            let mut list: Vec<(&String, &ProjectEntry)> = reg.projects.iter().collect();
            list.sort_by_key(|(k, _)| k.as_str());
            list.into_iter().map(|(name, entry)| json!({
                "name":   name,
                "target": entry.target.display().to_string(),
                "db":     entry.db.display().to_string(),
            })).collect()
        })
        .unwrap_or_default();
    let payload = json!({ "count": projects.len(), "projects": projects });
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload)? }],
        "isError": false,
    }))
}

/// Read lines [start_line..=end_line] (1-indexed) from a file in the target tree.
fn read_source(target: &Path, file: &str, start_line: i64, end_line: i64) -> Option<String> {
    let path = target.join(file);
    let content = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let s = ((start_line - 1) as usize).min(lines.len());
    let e = (end_line as usize).min(lines.len());
    if s >= e { return None; }
    Some(lines[s..e].join("\n"))
}

/// Enrich a symbol JSON object with its `source` field read from disk.
fn with_source(mut snippet: Value, target: &Path) -> Value {
    let file  = snippet.get("file").and_then(|v| v.as_str()).map(String::from);
    let start = snippet.get("start_line").and_then(|v| v.as_i64());
    let end   = snippet.get("end_line").and_then(|v| v.as_i64());
    if let (Some(f), Some(s), Some(e)) = (file, start, end) {
        if let Some(src) = read_source(target, &f, s, e) {
            snippet.as_object_mut().unwrap().insert("source".into(), json!(src));
        }
    }
    snippet
}

// --- tool implementations ----------------------------------------------------

fn resolve_symbol(conn: &Connection, sym: &str) -> Result<i64> {
    if let Some((file, line)) = sym.rsplit_once(':') {
        if let Ok(line_num) = line.parse::<i64>() {
            if let Ok(id) = conn.query_row(
                "SELECT li.symbol_id FROM line_index li
                 JOIN files f ON f.id = li.file_id
                 WHERE f.path = ?1 AND li.line = ?2 AND li.symbol_id IS NOT NULL
                 LIMIT 1",
                params![file, line_num], |r| r.get::<_,i64>(0),
            ) { return Ok(id); }
        }
    }
    conn.query_row(
        "SELECT id FROM symbols WHERE qualified_name = ?1 OR name = ?1 LIMIT 1",
        params![sym], |r| r.get::<_,i64>(0),
    ).map_err(|e| anyhow!("symbol not found: {sym} ({e})"))
}

fn symbol_snippet(conn: &Connection, sym_id: i64) -> Result<Value> {
    conn.query_row(
        "SELECT s.id, s.qualified_name, s.kind, s.signature, s.start_line, s.end_line,
                f.path, f.language
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1",
        params![sym_id],
        |r| Ok(json!({
            "id":        r.get::<_,i64>(0)?,
            "qualified": r.get::<_,String>(1)?,
            "kind":      r.get::<_,String>(2)?,
            "signature": r.get::<_,Option<String>>(3)?,
            "start_line":r.get::<_,i64>(4)?,
            "end_line":  r.get::<_,i64>(5)?,
            "file":      r.get::<_,String>(6)?,
            "language":  r.get::<_,String>(7)?,
        })),
    ).map_err(Into::into)
}

fn get_surgical_context(conn: &Connection, args: &Value, target: &Path) -> Result<Value> {
    let symbol = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 5);
    let max   = args.get("max_snippets").and_then(|v| v.as_i64()).unwrap_or(30).clamp(1, 200);

    let root_id = resolve_symbol(conn, symbol)?;

    let mut seen: std::collections::BTreeSet<i64> = [root_id].into_iter().collect();
    let mut frontier = vec![root_id];
    let mut out_ids: Vec<i64> = vec![root_id];

    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            let mut stmt = conn.prepare(
                "SELECT target_symbol_id FROM relations
                 WHERE source_symbol_id = ?1 AND target_symbol_id IS NOT NULL")?;
            for row in stmt.query_map(params![id], |r| r.get::<_,i64>(0))? {
                if let Ok(t) = row { if seen.insert(t) { next.push(t); out_ids.push(t); } }
            }
            let mut stmt = conn.prepare(
                "SELECT source_symbol_id FROM relations WHERE target_symbol_id = ?1")?;
            for row in stmt.query_map(params![id], |r| r.get::<_,i64>(0))? {
                if let Ok(t) = row { if seen.insert(t) { next.push(t); out_ids.push(t); } }
            }
            if out_ids.len() as i64 >= max { break; }
        }
        frontier = next;
        if frontier.is_empty() || (out_ids.len() as i64) >= max { break; }
    }
    out_ids.truncate(max as usize);

    let root = with_source(symbol_snippet(conn, root_id)?, target);
    let snippets: Vec<Value> = out_ids.iter()
        .filter_map(|id| symbol_snippet(conn, *id).ok())
        .map(|s| with_source(s, target))
        .collect();

    Ok(json!({
        "root":     root,
        "depth":    depth,
        "count":    snippets.len(),
        "snippets": snippets,
    }))
}

fn patch_symbol(conn: &Connection, args: &Value, target: &Path) -> Result<Value> {
    let symbol     = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let new_source = args.get("new_source").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'new_source' required"))?;

    let id      = resolve_symbol(conn, symbol)?;
    let snippet = symbol_snippet(conn, id)?;

    let file       = snippet.get("file").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("symbol has no file"))?;
    let start_line = snippet.get("start_line").and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("symbol has no start_line"))? as usize;
    let end_line   = snippet.get("end_line").and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("symbol has no end_line"))? as usize;

    let full_path = target.join(file);
    let content   = std::fs::read_to_string(&full_path)
        .with_context(|| format!("reading {}", full_path.display()))?;
    let trailing_newline = content.ends_with('\n');

    let lines: Vec<&str> = content.lines().collect();
    let before = &lines[..start_line.saturating_sub(1)];
    let after  = if end_line < lines.len() { &lines[end_line..] } else { &[] };

    let mut parts: Vec<&str> = Vec::with_capacity(before.len() + 1 + after.len());
    parts.extend_from_slice(before);
    parts.push(new_source);
    parts.extend_from_slice(after);

    let mut new_content = parts.join("\n");
    if trailing_newline { new_content.push('\n'); }

    std::fs::write(&full_path, &new_content)
        .with_context(|| format!("writing {}", full_path.display()))?;

    Ok(json!({
        "patched":        true,
        "symbol":         snippet.get("qualified").cloned().unwrap_or(json!(symbol)),
        "file":           file,
        "lines_replaced": format!("{}-{}", start_line, end_line),
        "note":           "File written. The daemon reindexes automatically on the next watcher tick."
    }))
}

fn query_graph(conn: &Connection, args: &Value) -> Result<Value> {
    let name = args.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let kind = args.get("kind").and_then(|v| v.as_str()).map(|s| s.to_string());
    let file = args.get("file").and_then(|v| v.as_str()).map(|s| s.to_string());
    let limit: i64 = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50).clamp(1, 500);

    let mut sql = String::from(
        "SELECT s.id, s.qualified_name, s.kind, f.path, s.start_line, s.end_line
         FROM symbols s JOIN files f ON f.id = s.file_id WHERE 1=1"
    );
    let mut p: Vec<rusqlite::types::Value> = vec![];
    if let Some(n) = &name { sql.push_str(" AND (s.name LIKE ?  OR s.qualified_name LIKE ?)"); let pat = format!("%{n}%"); p.push(pat.clone().into()); p.push(pat.into()); }
    if let Some(k) = &kind { sql.push_str(" AND s.kind = ?"); p.push(k.clone().into()); }
    if let Some(f) = &file { sql.push_str(" AND f.path LIKE ?"); p.push(format!("{f}%").into()); }
    sql.push_str(" ORDER BY s.kind, s.qualified_name LIMIT ?");
    p.push(limit.into());

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(p.iter()), |r| Ok(json!({
        "id": r.get::<_,i64>(0)?,
        "qualified": r.get::<_,String>(1)?,
        "kind": r.get::<_,String>(2)?,
        "file": r.get::<_,String>(3)?,
        "start_line": r.get::<_,i64>(4)?,
        "end_line": r.get::<_,i64>(5)?,
    })))?;
    let list: Vec<Value> = rows.filter_map(Result::ok).collect();
    Ok(json!({ "count": list.len(), "results": list }))
}

fn get_symbol(conn: &Connection, args: &Value) -> Result<Value> {
    let sym = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let id = resolve_symbol(conn, sym)?;
    symbol_snippet(conn, id)
}

fn get_callers(conn: &Connection, args: &Value) -> Result<Value> {
    let sym = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(2).clamp(1,5);
    let root = resolve_symbol(conn, sym)?;

    let mut seen: std::collections::BTreeSet<i64> = [root].into_iter().collect();
    let mut frontier = vec![root];
    let mut callers = Vec::<i64>::new();
    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            let mut stmt = conn.prepare(
                "SELECT source_symbol_id FROM relations WHERE target_symbol_id = ?1")?;
            for row in stmt.query_map(params![id], |r| r.get::<_,i64>(0))? {
                if let Ok(c) = row { if seen.insert(c) { next.push(c); callers.push(c); } }
            }
        }
        frontier = next;
    }
    let snippets: Vec<Value> = callers.iter().filter_map(|id| symbol_snippet(conn, *id).ok()).collect();
    Ok(json!({ "root": symbol_snippet(conn, root)?, "callers": snippets }))
}

fn get_callees(conn: &Connection, args: &Value) -> Result<Value> {
    let sym = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let root = resolve_symbol(conn, sym)?;
    let mut stmt = conn.prepare(
        "SELECT r.target_symbol_id, r.target_name, r.relation_kind, r.line
         FROM relations r WHERE r.source_symbol_id = ?1")?;
    let rows = stmt.query_map(params![root], |r| Ok(json!({
        "target_id":   r.get::<_,Option<i64>>(0)?,
        "target_name": r.get::<_,Option<String>>(1)?,
        "kind":        r.get::<_,String>(2)?,
        "line":        r.get::<_,i64>(3)?,
    })))?;
    Ok(json!({
        "root":    symbol_snippet(conn, root)?,
        "callees": rows.filter_map(Result::ok).collect::<Vec<_>>(),
    }))
}

fn graph_stats(conn: &Connection) -> Result<Value> {
    let files: i64     = conn.query_row("SELECT COUNT(*) FROM files",    [], |r| r.get(0))?;
    let symbols: i64   = conn.query_row("SELECT COUNT(*) FROM symbols",  [], |r| r.get(0))?;
    let relations: i64 = conn.query_row("SELECT COUNT(*) FROM relations",[], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY 2 DESC")?;
    let by_lang: Vec<Value> = stmt.query_map([], |r| Ok(json!({
        "language": r.get::<_,String>(0)?, "files": r.get::<_,i64>(1)?,
    })))?.filter_map(Result::ok).collect();
    Ok(json!({
        "files": files, "symbols": symbols, "relations": relations,
        "by_language": by_lang,
    }))
}
