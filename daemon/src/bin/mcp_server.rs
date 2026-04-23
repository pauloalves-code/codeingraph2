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
//!   * `get_surgical_context`   surgical slices of code impacted by a symbol
//!   * `query_graph`            search by name / kind / file
//!   * `get_symbol`             full metadata for one symbol
//!   * `get_callers`            who calls symbol X
//!   * `get_callees`            what does symbol X call
//!   * `graph_stats`            counts
//!
//! The server opens the SQLite DB in read-only mode; the daemon owns writes.

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

fn main() -> Result<()> {
    // Tracing to stderr (stdout is the MCP transport).
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();

    let db_path: PathBuf = std::env::var("CODEINGRAPH2_DB")
        .unwrap_or_else(|_| "/var/lib/codeingraph2/graph.db".into())
        .into();

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

        let resp = handle(&db_path, &req);
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

fn handle(db_path: &PathBuf, req: &Request) -> Result<Value> {
    if req.jsonrpc != "2.0" && !req.jsonrpc.is_empty() {
        return Err(anyhow!("unsupported jsonrpc version"));
    }
    match req.method.as_str() {
        "initialize"  => Ok(initialize()),
        "tools/list"  => Ok(tools_list()),
        "tools/call"  => tools_call(db_path, &req.params),
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
            "## ALWAYS use these tools BEFORE reading files:\n",
            "1. `query_graph` — find the exact file:line for any symbol by name/kind.\n",
            "2. `get_symbol` — get signature + location without opening the file.\n",
            "3. `get_surgical_context` — get ONLY the affected snippets before a refactor (never read whole files).\n\n",
            "## Workflow (saves 60-90% tokens vs raw file reads):\n",
            "- Need a symbol location? → `get_symbol name`\n",
            "- Need to understand what calls X? → `get_callers X depth=1`\n",
            "- Need to refactor X? → `get_surgical_context X depth=2` then edit only the returned file:lines.\n",
            "- Exploring an unknown codebase? → `graph_stats` then `query_graph kind=file` to see structure.\n\n",
            "## Rules:\n",
            "- NEVER read an entire file when you only need a function — use `get_symbol` + `Read(offset, limit)` on the exact lines.\n",
            "- NEVER grep the whole repo for a symbol name — `query_graph name=X` is instant and exact.\n",
            "- depth=1 is almost always enough; only use depth=2+ for deep refactors."
        )
    })
}

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "get_surgical_context",
            "description": "TOKEN SAVER: Returns ONLY the code snippets relevant to a symbol — definition + transitive callers/callees up to `depth`. Use this BEFORE any refactor instead of reading whole files. Returns file:start_line-end_line for each snippet so you can do targeted reads.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Qualified name (e.g. 'MyModule::my_fn'), plain name, or 'path/to/file.rs:42' for line-based lookup."
                    },
                    "depth":  { "type": "integer", "minimum": 1, "maximum": 5, "default": 1, "description": "1 = direct callers only (fastest). 2 = transitive. Rarely need >2." },
                    "max_snippets": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "query_graph",
            "description": "TOKEN SAVER: Find symbols by name/kind/file — returns file + exact line numbers. Use INSTEAD of grep. Example: {name:'authenticate', kind:'method'} finds the method without reading any file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name":  { "type": "string", "description": "Substring match against symbol name and qualified name." },
                    "kind":  { "type": "string", "enum": ["file","class","function","method","variable","constant","enum","trait","module"] },
                    "file":  { "type": "string", "description": "File path prefix filter (e.g. 'src/web')." },
                    "limit": { "type": "integer", "default": 50, "description": "Max results." }
                }
            }
        },
        {
            "name": "get_symbol",
            "description": "Full metadata for one symbol: signature, file, exact lines, visibility, docstring. Use this to get the precise location before a targeted Read(offset, limit) — avoids reading whole files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Qualified name or plain name." }
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
                    "symbol": { "type": "string" },
                    "depth":  { "type": "integer", "default": 2, "maximum": 5 }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "get_callees",
            "description": "What does symbol X call? Returns outbound edges with file:line. Use to understand dependencies before deleting or moving a symbol.",
            "inputSchema": {
                "type": "object",
                "properties": { "symbol": { "type": "string" } },
                "required": ["symbol"]
            }
        },
        {
            "name": "graph_stats",
            "description": "Global graph counts (files, symbols, relations) and per-language breakdown. Use first when exploring an unknown codebase to understand its size and structure.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ]})
}

fn tools_call(db_path: &PathBuf, params: &Value) -> Result<Value> {
    let name = params.get("name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call: missing 'name'"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let conn = codeingraph2::db::open_readonly(db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;

    let payload = match name {
        "get_surgical_context" => get_surgical_context(&conn, &args)?,
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

// --- tool implementations -------------------------------------------------

fn resolve_symbol(conn: &Connection, sym: &str) -> Result<i64> {
    // Accepts either "qualified::name" or "path/to/file.rs:LINE".
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

fn get_surgical_context(conn: &Connection, args: &Value) -> Result<Value> {
    let symbol = args.get("symbol").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("'symbol' required"))?;
    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 5);
    let max   = args.get("max_snippets").and_then(|v| v.as_i64()).unwrap_or(30).clamp(1, 200);

    let root_id = resolve_symbol(conn, symbol)?;

    // BFS both up (callers) and down (callees) up to depth
    let mut seen: std::collections::BTreeSet<i64> = [root_id].into_iter().collect();
    let mut frontier = vec![root_id];
    let mut out_ids: Vec<i64> = vec![root_id];

    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            // callees
            let mut stmt = conn.prepare(
                "SELECT target_symbol_id FROM relations
                 WHERE source_symbol_id = ?1 AND target_symbol_id IS NOT NULL")?;
            for row in stmt.query_map(params![id], |r| r.get::<_,i64>(0))? {
                if let Ok(t) = row { if seen.insert(t) { next.push(t); out_ids.push(t); } }
            }
            // callers
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

    let snippets: Vec<Value> = out_ids.iter()
        .filter_map(|id| symbol_snippet(conn, *id).ok())
        .collect();

    Ok(json!({
        "root":   symbol_snippet(conn, root_id)?,
        "depth":  depth,
        "count":  snippets.len(),
        "snippets": snippets,
        "hint": "Open each snippet at file:start_line to see exact code."
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
    let files: i64     = conn.query_row("SELECT COUNT(*) FROM files",     [], |r| r.get(0))?;
    let symbols: i64   = conn.query_row("SELECT COUNT(*) FROM symbols",   [], |r| r.get(0))?;
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
