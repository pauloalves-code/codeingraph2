//! Recompute fan_in / fan_out / centrality from the relations table.
//!
//! Centrality is a cheap proxy: normalized (fan_in + fan_out) — good enough to
//! rank symbols for the surgical-context query. Betweenness-centrality can
//! replace this later without changing callers.

use anyhow::Result;
use rusqlite::params;

use crate::db::Pool;

pub fn recompute(pool: &Pool) -> Result<()> {
    pool.with_conn_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM impact_scores", [])?;
        tx.execute(
            "INSERT INTO impact_scores (symbol_id, fan_in, fan_out, centrality, updated_at)
             SELECT s.id,
                    COALESCE((SELECT COUNT(*) FROM relations r WHERE r.target_symbol_id = s.id), 0),
                    COALESCE((SELECT COUNT(*) FROM relations r WHERE r.source_symbol_id = s.id), 0),
                    0.0,
                    strftime('%s','now')
             FROM symbols s",
            [],
        )?;
        // normalize centrality = (fan_in + fan_out) / max_total
        let max_total: i64 = tx.query_row(
            "SELECT COALESCE(MAX(fan_in + fan_out), 1) FROM impact_scores",
            [], |r| r.get(0)
        )?;
        tx.execute(
            "UPDATE impact_scores
               SET centrality = CAST(fan_in + fan_out AS REAL) / CAST(?1 AS REAL)",
            params![max_total.max(1)],
        )?;
        tx.commit()?;
        Ok(())
    })
}
