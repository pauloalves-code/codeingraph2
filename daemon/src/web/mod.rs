//! Embedded web UI: force-directed graph viewer (Obsidian-style) served on
//! a user-configurable port, protected by HTTP Basic Auth.
//!
//! Routes:
//!   GET /                 -> single-page viewer (HTML, inlined at compile time)
//!   GET /healthz          -> "ok"
//!   GET /api/graph        -> { nodes, edges } (filterable)
//!   GET /api/node/:id     -> full symbol metadata + source snippet
//!   GET /api/edge/:id     -> relation metadata + line context
//!   GET /api/source       -> raw source snippet by ?file=&start=&end=
//!   GET /api/stats        -> counts

pub mod auth;

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::config::Config;
use crate::db::Pool;

const INDEX_HTML: &str = include_str!("../../static/index.html");

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub cfg: Arc<Config>,
    pub auth: Arc<auth::AuthConfig>,
}

pub async fn serve(cfg: Config, pool: Pool) -> Result<()> {
    if !cfg.web_enabled {
        tracing::info!("web UI disabled (WEB_ENABLED=0)");
        return Ok(());
    }
    let auth = auth::AuthConfig::load(&cfg);
    if auth.is_anonymous() {
        tracing::warn!("web UI running WITHOUT authentication — set WEB_USER / WEB_AUTH to enable");
    } else {
        tracing::info!(user = %auth.username.as_deref().unwrap_or(""), "web UI auth enabled");
    }
    let state = AppState { pool, cfg: Arc::new(cfg.clone()), auth: Arc::new(auth) };

    let app = Router::new()
        .route("/",              get(index))
        .route("/api/graph",     get(api_graph))
        .route("/api/node/:id",  get(api_node))
        .route("/api/edge/:id",  get(api_edge))
        .route("/api/source",    get(api_source))
        .route("/api/stats",     get(api_stats))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::basic_auth))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    let addr: SocketAddr = cfg.web_bind.parse()
        .unwrap_or_else(|_| "0.0.0.0:7890".parse().unwrap());
    tracing::info!("web UI listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// --- handlers --------------------------------------------------------------

async fn index() -> Response {
    axum::response::Html(INDEX_HTML).into_response()
}

#[derive(Deserialize)]
struct GraphQuery {
    /// `0` (or missing) means "no limit".
    #[serde(default)] limit: usize,
    kind: Option<String>,
    q:    Option<String>,   // name search
}

async fn api_graph(
    State(st): State<AppState>,
    Query(q):  Query<GraphQuery>,
) -> Result<Json<Value>, ApiError> {
    let nodes_and_edges = st.pool.with_conn(|c| {
        // ── 1. Primary nodes (with kind / search filters) ──────────────────
        let mut sql = String::from(
            "SELECT s.id, s.name, s.qualified_name, s.kind, f.path, s.start_line, s.end_line,
                    COALESCE(i.fan_in,0), COALESCE(i.fan_out,0)
             FROM symbols s
             JOIN files f  ON f.id = s.file_id
             LEFT JOIN impact_scores i ON i.symbol_id = s.id
             WHERE 1=1"
        );
        let mut p: Vec<rusqlite::types::Value> = vec![];
        if let Some(k) = &q.kind { sql.push_str(" AND s.kind = ?"); p.push(k.clone().into()); }
        if let Some(s) = &q.q {
            sql.push_str(" AND (s.name LIKE ? OR s.qualified_name LIKE ?)");
            let pat = format!("%{s}%"); p.push(pat.clone().into()); p.push(pat.into());
        }
        sql.push_str(" ORDER BY (COALESCE(i.fan_in,0) + COALESCE(i.fan_out,0)) DESC");
        if q.limit > 0 {
            sql.push_str(" LIMIT ?");
            p.push((q.limit as i64).into());
        }

        let mut stmt = c.prepare(&sql)?;
        let primary: Vec<Value> = stmt.query_map(
            rusqlite::params_from_iter(p.iter()),
            |r| Ok(json!({
                "id":       r.get::<_,i64>(0)?,
                "name":     r.get::<_,String>(1)?,
                "qname":    r.get::<_,String>(2)?,
                "kind":     r.get::<_,String>(3)?,
                "file":     r.get::<_,String>(4)?,
                "start":    r.get::<_,i64>(5)?,
                "end":      r.get::<_,i64>(6)?,
                "fan_in":   r.get::<_,i64>(7)?,
                "fan_out":  r.get::<_,i64>(8)?,
            })),
        )?.filter_map(Result::ok).collect();

        if primary.is_empty() {
            return Ok(json!({ "nodes": [], "edges": [] }));
        }

        let primary_ids: Vec<i64> = primary.iter()
            .filter_map(|n| n.get("id").and_then(|v| v.as_i64()))
            .collect();
        let ph = std::iter::repeat("?").take(primary_ids.len()).collect::<Vec<_>>().join(",");
        let id_vals: Vec<rusqlite::types::Value> = primary_ids.iter().map(|&id| id.into()).collect();

        // ── 2. Edges ────────────────────────────────────────────────────────
        // Always strict: only show edges where BOTH endpoints are in the
        // primary (filtered) set. When a kind filter is active this keeps
        // only same-kind cross-edges; neighbours of other kinds can be
        // explored via the "Expandir conexões" feature in the UI.
        let edge_sql = format!(
            "SELECT id, source_symbol_id, target_symbol_id, relation_kind, line, weight
             FROM relations
             WHERE target_symbol_id IS NOT NULL
               AND source_symbol_id IN ({ph})
               AND target_symbol_id IN ({ph})",
            ph = ph,
        );
        let edge_params: Vec<rusqlite::types::Value> =
            id_vals.iter().chain(id_vals.iter()).cloned().collect();

        let mut stmt = c.prepare(&edge_sql)?;
        let edges: Vec<Value> = stmt.query_map(
            rusqlite::params_from_iter(edge_params.iter()),
            |r| Ok(json!({
                "id":     r.get::<_,i64>(0)?,
                "source": r.get::<_,i64>(1)?,
                "target": r.get::<_,i64>(2)?,
                "kind":   r.get::<_,String>(3)?,
                "line":   r.get::<_,i64>(4)?,
                "weight": r.get::<_,f64>(5)?,
            })),
        )?.filter_map(Result::ok).collect();

        // ── 3. Nodes ────────────────────────────────────────────────────────
        // No secondary-node expansion: the primary set is the final node list.
        // (Use "Expandir conexões ao clicar" in the UI to explore neighbours.)
        let nodes = primary;

        Ok(json!({ "nodes": nodes, "edges": edges }))
    })?;
    Ok(Json(nodes_and_edges))
}

async fn api_node(
    State(st): State<AppState>,
    Path(id):  Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let meta = st.pool.with_conn(|c| {
        let (qname, kind, sig, vis, doc, file, lang, start, end): (
            String, String, Option<String>, Option<String>, Option<String>,
            String, String, i64, i64,
        ) = c.query_row(
            "SELECT s.qualified_name, s.kind, s.signature, s.visibility, s.docstring,
                    f.path, f.language, s.start_line, s.end_line
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                    r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )?;

        // outgoing
        let mut stmt = c.prepare(
            "SELECT r.id, r.relation_kind, r.target_symbol_id, r.target_name, r.line, s2.qualified_name
             FROM relations r
             LEFT JOIN symbols s2 ON s2.id = r.target_symbol_id
             WHERE r.source_symbol_id = ?1 ORDER BY r.line")?;
        let outgoing: Vec<Value> = stmt.query_map(params![id], |r| Ok(json!({
            "id":           r.get::<_,i64>(0)?,
            "kind":         r.get::<_,String>(1)?,
            "target_id":    r.get::<_,Option<i64>>(2)?,
            "target_name":  r.get::<_,Option<String>>(3)?,
            "line":         r.get::<_,i64>(4)?,
            "target_qname": r.get::<_,Option<String>>(5)?,
        })))?.filter_map(Result::ok).collect();

        // incoming
        let mut stmt = c.prepare(
            "SELECT r.id, r.relation_kind, r.source_symbol_id, s2.qualified_name, r.line
             FROM relations r JOIN symbols s2 ON s2.id = r.source_symbol_id
             WHERE r.target_symbol_id = ?1 ORDER BY r.line")?;
        let incoming: Vec<Value> = stmt.query_map(params![id], |r| Ok(json!({
            "id":           r.get::<_,i64>(0)?,
            "kind":         r.get::<_,String>(1)?,
            "source_id":    r.get::<_,i64>(2)?,
            "source_qname": r.get::<_,String>(3)?,
            "line":         r.get::<_,i64>(4)?,
        })))?.filter_map(Result::ok).collect();

        Ok(json!({
            "id": id, "qname": qname, "kind": kind,
            "signature": sig, "visibility": vis, "docstring": doc,
            "file": file, "language": lang,
            "start_line": start, "end_line": end,
            "outgoing": outgoing, "incoming": incoming,
        }))
    })?;

    // Attach the source snippet (best effort; missing file is fine)
    let file_rel = meta.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let start = meta.get("start_line").and_then(|v| v.as_i64()).unwrap_or(1);
    let end   = meta.get("end_line").and_then(|v| v.as_i64()).unwrap_or(start);
    let src = read_slice(&st.cfg.target, &file_rel, start, end).unwrap_or_default();
    let mut m = meta;
    m["source"] = Value::String(src);
    Ok(Json(m))
}

async fn api_edge(
    State(st): State<AppState>,
    Path(id):  Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let v = st.pool.with_conn(|c| {
        let (src_id, tgt_id, tgt_name, kind, line, src_q, tgt_q, file): (
            i64, Option<i64>, Option<String>, String, i64, String, Option<String>, String,
        ) = c.query_row(
            "SELECT r.source_symbol_id, r.target_symbol_id, r.target_name, r.relation_kind, r.line,
                    s1.qualified_name, s2.qualified_name, f.path
             FROM relations r
             JOIN symbols s1 ON s1.id = r.source_symbol_id
             JOIN files   f  ON f.id  = s1.file_id
             LEFT JOIN symbols s2 ON s2.id = r.target_symbol_id
             WHERE r.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                    r.get(5)?, r.get(6)?, r.get(7)?)),
        )?;
        Ok(json!({
            "id": id, "kind": kind, "line": line,
            "source": { "id": src_id, "qname": src_q, "file": file },
            "target": { "id": tgt_id, "qname": tgt_q, "name": tgt_name },
        }))
    })?;
    // Attach a 5-line-context snippet around the edge line.
    let file_rel = v["source"]["file"].as_str().unwrap_or("").to_string();
    let line = v["line"].as_i64().unwrap_or(1);
    let ctx_start = (line - 2).max(1);
    let ctx_end   = line + 2;
    let src = read_slice(&st.cfg.target, &file_rel, ctx_start, ctx_end).unwrap_or_default();
    let mut v = v;
    v["context"] = json!({ "start": ctx_start, "end": ctx_end, "source": src });
    Ok(Json(v))
}

#[derive(Deserialize)]
struct SourceQuery { file: String, start: i64, end: i64 }

async fn api_source(
    State(st): State<AppState>,
    Query(q):  Query<SourceQuery>,
) -> Result<Json<Value>, ApiError> {
    let src = read_slice(&st.cfg.target, &q.file, q.start, q.end)?;
    Ok(Json(json!({ "file": q.file, "start": q.start, "end": q.end, "source": src })))
}

async fn api_stats(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let v = st.pool.with_conn(|c| {
        let files: i64     = c.query_row("SELECT COUNT(*) FROM files",    [], |r| r.get(0))?;
        let symbols: i64   = c.query_row("SELECT COUNT(*) FROM symbols",  [], |r| r.get(0))?;
        let relations: i64 = c.query_row("SELECT COUNT(*) FROM relations",[], |r| r.get(0))?;
        let mut stmt = c.prepare("SELECT kind, COUNT(*) FROM symbols GROUP BY kind")?;
        let by_kind: Vec<Value> = stmt.query_map([], |r| Ok(json!({
            "kind": r.get::<_,String>(0)?, "count": r.get::<_,i64>(1)?,
        })))?.filter_map(Result::ok).collect();
        Ok(json!({
            "files": files, "symbols": symbols, "relations": relations,
            "by_kind": by_kind,
        }))
    })?;
    let mut v = v;
    v["project"] = Value::String(st.cfg.project_name.clone());
    Ok(Json(v))
}

// --- helpers --------------------------------------------------------------

/// Read lines [start, end] (1-based, inclusive) from `target_root / rel_path`.
/// Guards against path traversal without relying on canonicalize() — which
/// can fail or return unexpected paths on bind-mounted Docker volumes.
fn read_slice(target_root: &std::path::Path, rel_path: &str, start: i64, end: i64) -> Result<String> {
    // Reject obviously dangerous paths before joining.
    if rel_path.contains("..") || rel_path.starts_with('/') {
        anyhow::bail!("unsafe path: {rel_path}");
    }
    let path = target_root.join(rel_path);

    // Secondary check: normalised string prefix (handles any remaining edge cases).
    let root_str = target_root.to_string_lossy();
    let path_str = path.to_string_lossy();
    if !path_str.starts_with(root_str.as_ref()) {
        anyhow::bail!("path escapes target root");
    }

    let content = std::fs::read_to_string(&path)?;
    let s = (start.max(1) - 1) as usize;
    let e = end.max(start) as usize;
    // Collect lines s..=e-1 (0-based) = start..=end (1-based)
    let out: Vec<&str> = content.lines().enumerate()
        .skip(s)
        .take(e - s)
        .map(|(_, l)| l)
        .collect();
    Ok(out.join("\n"))
}

// --- error type ------------------------------------------------------------

pub struct ApiError(pub anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for ApiError { fn from(e: E) -> Self { ApiError(e.into()) } }
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(error = %self.0, "api error");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": self.0.to_string() })))
            .into_response()
    }
}

