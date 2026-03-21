//! SQLite mirror of token-history.jsonl for fast queries.
//!
//! Tables `i`, `f`, `w` mirror the three JSONL record types.
//! Views `instance_compare` and `promo_check` provide derived analytics.
//! `rebuild_from_jsonl()` reconstructs the DB from the authoritative JSONL.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::fs;
use std::io::BufRead;
use std::path::Path;

/// Open (or create) the SQLite database at the given path.
pub fn open_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open SQLite database: {}", db_path.display()))?;
    Ok(conn)
}

/// Create all tables, indexes, and views for the token history mirror.
pub fn create_schema(conn: &Connection) -> Result<()> {
    // Table i: instance records
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS i (
            r         TEXT NOT NULL,
            ts        TEXT NOT NULL,
            t0        TEXT NOT NULL,
            t1        TEXT NOT NULL,
            sess      TEXT NOT NULL,
            sid       TEXT NOT NULL,
            model     TEXT NOT NULL,
            pk        INTEGER NOT NULL DEFAULT 0,
            hr_et     INTEGER NOT NULL DEFAULT 0,
            dow       INTEGER NOT NULL DEFAULT 0,
            input_n   INTEGER NOT NULL DEFAULT 0,
            input_usd REAL NOT NULL DEFAULT 0.0,
            output_n  INTEGER NOT NULL DEFAULT 0,
            output_usd REAL NOT NULL DEFAULT 0.0,
            r_cache_n INTEGER NOT NULL DEFAULT 0,
            r_cache_usd REAL NOT NULL DEFAULT 0.0,
            w_cache_n INTEGER NOT NULL DEFAULT 0,
            w_cache_usd REAL NOT NULL DEFAULT 0.0,
            w_cache_1h_n INTEGER NOT NULL DEFAULT 0,
            w_cache_1h_usd REAL NOT NULL DEFAULT 0.0,
            total_usd REAL NOT NULL DEFAULT 0.0,
            cache_eff REAL NOT NULL DEFAULT 0.0,
            p5h       REAL,
            p7d       REAL,
            p7ds      REAL
        );",
    )?;

    // Table f: fleet records
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS f (
            r         TEXT NOT NULL,
            ts        TEXT NOT NULL,
            t0        TEXT NOT NULL,
            t1        TEXT NOT NULL,
            pk        INTEGER NOT NULL DEFAULT 0,
            hr_et     INTEGER NOT NULL DEFAULT 0,
            dow       INTEGER NOT NULL DEFAULT 0,
            workers   INTEGER NOT NULL DEFAULT 0,
            total_usd REAL NOT NULL DEFAULT 0.0,
            p75_usd_hr REAL NOT NULL DEFAULT 0.0,
            std_usd_hr REAL NOT NULL DEFAULT 0.0,
            p5h       REAL,
            p7d       REAL,
            p7ds      REAL,
            usd_per_pct_7ds REAL,
            fleet_cache_eff REAL NOT NULL DEFAULT 0.0,
            cache_eff_p25   REAL NOT NULL DEFAULT 0.0,
            payload   TEXT NOT NULL DEFAULT '{}'
        );",
    )?;

    // Table w: window forecast records
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS w (
            r           TEXT NOT NULL,
            ts          TEXT NOT NULL,
            win         TEXT NOT NULL,
            pk          INTEGER NOT NULL DEFAULT 0,
            ceil        REAL NOT NULL DEFAULT 90.0,
            snap        REAL NOT NULL DEFAULT 0.0,
            reset       TEXT NOT NULL,
            delta       REAL NOT NULL DEFAULT 0.0,
            remain      REAL NOT NULL DEFAULT 0.0,
            hrs_left    REAL NOT NULL DEFAULT 0.0,
            fleet_pct_hr REAL NOT NULL DEFAULT 0.0,
            exh_hrs     REAL NOT NULL DEFAULT 0.0,
            cutoff_risk INTEGER NOT NULL DEFAULT 0,
            margin_hrs  REAL NOT NULL DEFAULT 0.0,
            bind        INTEGER NOT NULL DEFAULT 0,
            safe_w      INTEGER
        );",
    )?;

    // Indexes
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS i_t0_sess ON i(t0, sess);
         CREATE INDEX IF NOT EXISTS i_model_t0 ON i(model, t0);
         CREATE INDEX IF NOT EXISTS i_pk_t0 ON i(pk, t0);
         CREATE INDEX IF NOT EXISTS f_t0 ON f(t0);
         CREATE INDEX IF NOT EXISTS f_pk_t0 ON f(pk, t0);
         CREATE INDEX IF NOT EXISTS w_win_t0 ON w(win, ts);
         CREATE INDEX IF NOT EXISTS w_cutoff_risk ON w(cutoff_risk);",
    )?;

    // View: instance_compare — per-instance cost comparison
    conn.execute_batch(
        "CREATE VIEW IF NOT EXISTS instance_compare AS
         SELECT
             sess,
             model,
             t0,
             t1,
             total_usd,
             CASE WHEN (julianday(t1) - julianday(t0)) * 24 > 0
                  THEN total_usd / ((julianday(t1) - julianday(t0)) * 24)
                  ELSE 0 END AS usd_per_hour,
             CASE WHEN p7ds IS NOT NULL AND p7ds > 0
                  THEN total_usd / p7ds
                  ELSE NULL END AS usd_per_pct_7ds
         FROM i;",
    )?;

    // View: promo_check — peak vs off-peak cost comparison
    conn.execute_batch(
        "CREATE VIEW IF NOT EXISTS promo_check AS
         SELECT
             pk,
             hr_et,
             model,
             COUNT(*) AS instance_count,
             SUM(total_usd) AS total_usd,
             AVG(total_usd) AS avg_usd,
             CASE WHEN p7ds IS NOT NULL AND p7ds > 0
                  THEN SUM(total_usd) / p7ds
                  ELSE NULL END AS usd_per_pct_7ds
         FROM i
         GROUP BY pk, hr_et, model;",
    )?;

    // View: workspace_cache_eff — per-instance cache efficiency over time
    conn.execute_batch(
        "CREATE VIEW IF NOT EXISTS workspace_cache_eff AS
         SELECT
             sess,
             model,
             t0,
             t1,
             pk,
             hr_et,
             dow,
             cache_eff,
             input_n + r_cache_n AS total_input_n
         FROM i
         ORDER BY t0 DESC;",
    )?;

    Ok(())
}

/// Insert an instance record (type "i") into the SQLite mirror.
pub fn insert_instance(conn: &Connection, record: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO i (r, ts, t0, t1, sess, sid, model, pk, hr_et, dow,
                        input_n, input_usd, output_n, output_usd,
                        r_cache_n, r_cache_usd, w_cache_n, w_cache_usd,
                        w_cache_1h_n, w_cache_1h_usd, total_usd, cache_eff, p5h, p7d, p7ds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            record.get("r").and_then(|v| v.as_str()).unwrap_or("i"),
            record.get("ts").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("t0").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("t1").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("sess").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("sid").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("pk").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("hr_et").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("dow").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("input-n").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("input-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("output-n").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("output-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("r-cache-n").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("r-cache-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("w-cache-n").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("w-cache-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("w-cache-1h-n").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("w-cache-1h-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("total-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("cache-eff").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("p5h").and_then(|v| v.as_f64()),
            record.get("p7d").and_then(|v| v.as_f64()),
            record.get("p7ds").and_then(|v| v.as_f64()),
        ],
    )?;
    Ok(())
}

/// Insert a fleet record (type "f") into the SQLite mirror.
///
/// The full JSON payload is stored in the `payload` column for per-model column access.
pub fn insert_fleet(conn: &Connection, record: &serde_json::Value) -> Result<()> {
    let payload = serde_json::to_string(record).unwrap_or_default();
    conn.execute(
        "INSERT INTO f (r, ts, t0, t1, pk, hr_et, dow, workers,
                        total_usd, p75_usd_hr, std_usd_hr, p5h, p7d, p7ds,
                        usd_per_pct_7ds, fleet_cache_eff, cache_eff_p25, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                 ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            record.get("r").and_then(|v| v.as_str()).unwrap_or("f"),
            record.get("ts").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("t0").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("t1").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("pk").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("hr_et").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("dow").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("workers").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("total-usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("p75-usd-hr").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("std-usd-hr").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("p5h").and_then(|v| v.as_f64()),
            record.get("p7d").and_then(|v| v.as_f64()),
            record.get("p7ds").and_then(|v| v.as_f64()),
            record.get("usd-per-pct-7ds").and_then(|v| v.as_f64()),
            record.get("fleet-cache-eff").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("cache-eff-p25").and_then(|v| v.as_f64()).unwrap_or(0.0),
            payload,
        ],
    )?;
    Ok(())
}

/// Insert a window forecast record (type "w") into the SQLite mirror.
pub fn insert_window(conn: &Connection, record: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO w (r, ts, win, pk, ceil, snap, reset, delta, remain,
                        hrs_left, fleet_pct_hr, exh_hrs, cutoff_risk,
                        margin_hrs, bind, safe_w)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                 ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            record.get("r").and_then(|v| v.as_str()).unwrap_or("w"),
            record.get("ts").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("win").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("pk").and_then(|v| v.as_bool()).unwrap_or(false) as i64,
            record.get("ceil").and_then(|v| v.as_f64()).unwrap_or(90.0),
            record.get("snap").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("reset").and_then(|v| v.as_str()).unwrap_or(""),
            record.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("hrs_left").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("fleet_pct_hr").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("exh_hrs").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("cutoff_risk").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("margin_hrs").and_then(|v| v.as_f64()).unwrap_or(0.0),
            record.get("bind").and_then(|v| v.as_u64()).unwrap_or(0) as i64,
            record.get("safe_w").and_then(|v| v.as_u64()).map(|v| v as i64),
        ],
    )?;
    Ok(())
}

/// Insert a JSONL record into the appropriate table based on its `r` field.
pub fn insert_record(conn: &Connection, record: &serde_json::Value) -> Result<()> {
    let r = record.get("r").and_then(|v| v.as_str()).unwrap_or("");
    match r {
        "i" => insert_instance(conn, record),
        "f" => insert_fleet(conn, record),
        "w" => insert_window(conn, record),
        _ => Ok(()), // Skip unknown record types
    }
}

/// Rebuild the entire SQLite database from the JSONL source file.
///
/// Drops and recreates all tables, then reads every line from the JSONL
/// file and inserts it into the appropriate table.
pub fn rebuild_from_jsonl(jsonl_path: &Path, db_path: &Path) -> Result<usize> {
    let conn = open_db(db_path)?;

    // Drop and recreate schema
    conn.execute_batch("DROP TABLE IF EXISTS i; DROP TABLE IF EXISTS f; DROP TABLE IF EXISTS w;
                         DROP VIEW IF EXISTS instance_compare; DROP VIEW IF EXISTS promo_check;")?;
    create_schema(&conn)?;

    if !jsonl_path.exists() {
        return Ok(0);
    }

    let file = fs::File::open(jsonl_path)
        .with_context(|| format!("Failed to open JSONL: {}", jsonl_path.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut count = 0usize;

    let tx = conn.unchecked_transaction()?;
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Err(e) = insert_record(&tx, &record) {
                log::warn!("[db] skipping line {}: {}", count, e);
            } else {
                count += 1;
            }
        }
    }
    tx.commit()?;

    Ok(count)
}

/// Query the last N window records from the database.
pub fn query_last_windows(conn: &Connection, n: usize) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT r, ts, win, pk, ceil, snap, reset, delta, remain,
                hrs_left, fleet_pct_hr, exh_hrs, cutoff_risk,
                margin_hrs, bind, safe_w
         FROM w ORDER BY ts DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![n as i64], |row| {
        let pk: i64 = row.get(3)?;
        let safe_w: Option<i64> = row.get(15)?;
        Ok(serde_json::json!({
            "r": row.get::<_, String>(0)?,
            "ts": row.get::<_, String>(1)?,
            "win": row.get::<_, String>(2)?,
            "pk": pk != 0,
            "ceil": row.get::<_, f64>(4)?,
            "snap": row.get::<_, f64>(5)?,
            "reset": row.get::<_, String>(6)?,
            "delta": row.get::<_, f64>(7)?,
            "remain": row.get::<_, f64>(8)?,
            "hrs_left": row.get::<_, f64>(9)?,
            "fleet_pct_hr": row.get::<_, f64>(10)?,
            "exh_hrs": row.get::<_, f64>(11)?,
            "cutoff_risk": row.get::<_, i64>(12)?,
            "margin_hrs": row.get::<_, f64>(13)?,
            "bind": row.get::<_, i64>(14)?,
            "safe_w": safe_w,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Query instance_compare view for cross-instance comparison.
pub fn query_instance_compare(conn: &Connection, limit: usize) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT sess, model, t0, t1, total_usd, usd_per_hour, usd_per_pct_7ds
         FROM instance_compare ORDER BY total_usd DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(serde_json::json!({
            "sess": row.get::<_, String>(0)?,
            "model": row.get::<_, String>(1)?,
            "t0": row.get::<_, String>(2)?,
            "t1": row.get::<_, String>(3)?,
            "total_usd": row.get::<_, f64>(4)?,
            "usd_per_hour": row.get::<_, f64>(5)?,
            "usd_per_pct_7ds": row.get::<_, Option<f64>>(6)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Query the last N fleet records from the database.
pub fn query_last_fleets(conn: &Connection, n: usize) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT r, ts, t0, t1, pk, hr_et, dow, workers,
                total_usd, p75_usd_hr, std_usd_hr, p5h, p7d, p7ds,
                usd_per_pct_7ds, payload
         FROM f ORDER BY ts DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![n as i64], |row| {
        let payload: String = row.get(15)?;
        Ok(serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(serde_json::json!({
            "r": row.get::<_, String>(0)?,
            "ts": row.get::<_, String>(1)?,
            "error": "failed to parse payload",
        })))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Instance record for burn rate computation
#[derive(Debug, Clone)]
pub struct DbInstanceRecord {
    /// Session identifier
    pub session: String,
    /// Model identifier
    pub model: String,
    /// Total USD cost for this interval
    pub total_usd: f64,
    /// Total tokens consumed this interval
    pub total_tokens: u64,
    /// 5-hour window pct delta (may be null if not yet annotated)
    pub p5h: Option<f64>,
    /// 7-day window pct delta (may be null if not yet annotated)
    pub p7d: Option<f64>,
    /// 7-day sonnet window pct delta (may be null if not yet annotated)
    pub p7ds: Option<f64>,
    /// Current 5-hour utilization snapshot (approximated from delta)
    pub current_p5h: f64,
    /// Previous 5-hour utilization snapshot
    pub prev_p5h: f64,
    /// Current 7-day utilization snapshot
    pub current_p7d: f64,
    /// Previous 7-day utilization snapshot
    pub prev_p7d: f64,
    /// Current 7-day sonnet utilization snapshot
    pub current_p7ds: f64,
    /// Previous 7-day sonnet utilization snapshot
    pub prev_p7ds: f64,
}

/// Query instance records from the most recent interval for burn rate computation.
///
/// Returns all instance records from the last complete collection interval
/// that have been annotated with window percentage deltas by the governor.
pub fn query_instance_records_for_burn_rate(conn: &Connection) -> Result<Vec<DbInstanceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT sess, model, total_usd, input_n, output_n, r_cache_n, w_cache_n, w_cache_1h_n,
                p5h, p7d, p7ds
         FROM i
         WHERE p5h IS NOT NULL OR p7d IS NOT NULL OR p7ds IS NOT NULL
         ORDER BY t1 DESC
         LIMIT 100",
    )?;

    let rows = stmt.query_map([], |row| {
        let p5h: Option<f64> = row.get(8)?;
        let p7d: Option<f64> = row.get(9)?;
        let p7ds: Option<f64> = row.get(10)?;

        // Sum all token types for total
        let input_n: i64 = row.get(3)?;
        let output_n: i64 = row.get(4)?;
        let r_cache_n: i64 = row.get(5)?;
        let w_cache_n: i64 = row.get(6)?;
        let w_cache_1h_n: i64 = row.get(7)?;
        let total_tokens = (input_n + output_n + r_cache_n + w_cache_n + w_cache_1h_n) as u64;

        // Approximate current/previous utilization from deltas
        // (actual values come from governor's FleetAggregate)
        let current_p5h = p5h.unwrap_or(0.0);
        let prev_p5h = 0.0;
        let current_p7d = p7d.unwrap_or(0.0);
        let prev_p7d = 0.0;
        let current_p7ds = p7ds.unwrap_or(0.0);
        let prev_p7ds = 0.0;

        Ok(DbInstanceRecord {
            session: row.get(0)?,
            model: row.get(1)?,
            total_usd: row.get(2)?,
            total_tokens,
            p5h,
            p7d,
            p7ds,
            current_p5h,
            prev_p5h,
            current_p7d,
            prev_p7d,
            current_p7ds,
            prev_p7ds,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Query the last N instance records from the most recent interval.
///
/// Returns records ordered by t1 (interval end time) descending.
pub fn query_last_instances(conn: &Connection, n: usize) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT r, ts, t0, t1, sess, sid, model, pk, hr_et, dow,
                input_n, input_usd, output_n, output_usd,
                r_cache_n, r_cache_usd, w_cache_n, w_cache_usd,
                w_cache_1h_n, w_cache_1h_usd, total_usd, p5h, p7d, p7ds
         FROM i ORDER BY t1 DESC LIMIT ?1",
    )?;

    let rows = stmt.query_map(params![n as i64], |row| {
        Ok(serde_json::json!({
            "r": row.get::<_, String>(0)?,
            "ts": row.get::<_, String>(1)?,
            "t0": row.get::<_, String>(2)?,
            "t1": row.get::<_, String>(3)?,
            "sess": row.get::<_, String>(4)?,
            "sid": row.get::<_, String>(5)?,
            "model": row.get::<_, String>(6)?,
            "pk": row.get::<_, i64>(7)?,
            "hr_et": row.get::<_, i64>(8)?,
            "dow": row.get::<_, i64>(9)?,
            "input-n": row.get::<_, i64>(10)?,
            "input-usd": row.get::<_, f64>(11)?,
            "output-n": row.get::<_, i64>(12)?,
            "output-usd": row.get::<_, f64>(13)?,
            "r-cache-n": row.get::<_, i64>(14)?,
            "r-cache-usd": row.get::<_, f64>(15)?,
            "w-cache-n": row.get::<_, i64>(16)?,
            "w-cache-usd": row.get::<_, f64>(17)?,
            "w-cache-1h-n": row.get::<_, i64>(18)?,
            "w-cache-1h-usd": row.get::<_, f64>(19)?,
            "total-usd": row.get::<_, f64>(20)?,
            "p5h": row.get::<_, Option<f64>>(21)?,
            "p7d": row.get::<_, Option<f64>>(22)?,
            "p7ds": row.get::<_, Option<f64>>(23)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = open_db(&db_path).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn schema_creates_tables() {
        let (_temp, conn) = setup_db();

        // Verify tables exist by querying them
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM i", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM f", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM w", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn indexes_exist() {
        let (_temp, conn) = setup_db();

        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE '%_t0%' OR name LIKE '%_pk_%' OR name LIKE '%_win_%' OR name LIKE '%_cutoff%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(indexes.iter().any(|i| i == "i_t0_sess"), "i_t0_sess index missing");
        assert!(indexes.iter().any(|i| i == "i_model_t0"), "i_model_t0 index missing");
        assert!(indexes.iter().any(|i| i == "i_pk_t0"), "i_pk_t0 index missing");
        assert!(indexes.iter().any(|i| i == "f_t0"), "f_t0 index missing");
        assert!(indexes.iter().any(|i| i == "f_pk_t0"), "f_pk_t0 index missing");
        assert!(indexes.iter().any(|i| i == "w_win_t0"), "w_win_t0 index missing");
        assert!(indexes.iter().any(|i| i == "w_cutoff_risk"), "w_cutoff_risk index missing");
    }

    #[test]
    fn insert_and_query_instance() {
        let (_temp, conn) = setup_db();

        let record = serde_json::json!({
            "r": "i",
            "ts": "2026-03-20T10:00:00Z",
            "t0": "2026-03-20T09:55:00Z",
            "t1": "2026-03-20T10:00:00Z",
            "sess": "worker-a",
            "sid": "abc123",
            "model": "claude-sonnet-4-20250514",
            "pk": 1,
            "hr_et": 10,
            "dow": 2,
            "input-n": 1000,
            "input-usd": 3.0,
            "output-n": 500,
            "output-usd": 7.5,
            "r-cache-n": 200,
            "r-cache-usd": 0.06,
            "w-cache-n": 100,
            "w-cache-usd": 0.375,
            "w-cache-1h-n": 50,
            "w-cache-1h-usd": 0.3,
            "total-usd": 11.235,
        });

        insert_record(&conn, &record).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM i WHERE sess = 'worker-a'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_and_query_fleet() {
        let (_temp, conn) = setup_db();

        let record = serde_json::json!({
            "r": "f",
            "ts": "2026-03-20T10:00:00Z",
            "t0": "2026-03-20T09:55:00Z",
            "t1": "2026-03-20T10:00:00Z",
            "pk": 1,
            "hr_et": 10,
            "dow": 2,
            "workers": 2,
            "total-usd": 22.47,
            "p75-usd-hr": 5.0,
            "std-usd-hr": 1.2,
        });

        insert_record(&conn, &record).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM f", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_and_query_window() {
        let (_temp, conn) = setup_db();

        let record = serde_json::json!({
            "r": "w",
            "ts": "2026-03-20T10:00:00Z",
            "win": "five_hour",
            "pk": true,
            "ceil": 90.0,
            "snap": 36.0,
            "reset": "2026-03-20T13:00:00Z",
            "delta": 2.0,
            "remain": 54.0,
            "hrs_left": 3.0,
            "fleet_pct_hr": 2.0,
            "exh_hrs": 27.0,
            "cutoff_risk": 0,
            "margin_hrs": -24.0,
            "bind": 1,
            "safe_w": 5,
        });

        insert_record(&conn, &record).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM w WHERE win = 'five_hour'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn instance_compare_view_works() {
        let (_temp, conn) = setup_db();

        // Insert two instance records
        let rec1 = serde_json::json!({
            "r": "i", "ts": "2026-03-20T10:00:00Z",
            "t0": "2026-03-20T09:00:00Z", "t1": "2026-03-20T10:00:00Z",
            "sess": "a", "sid": "a", "model": "sonnet", "pk": 1, "hr_et": 10, "dow": 2,
            "input-n": 0, "input-usd": 0, "output-n": 0, "output-usd": 0,
            "r-cache-n": 0, "r-cache-usd": 0, "w-cache-n": 0, "w-cache-usd": 0,
            "w-cache-1h-n": 0, "w-cache-1h-usd": 0, "total-usd": 10.0,
        });
        let rec2 = serde_json::json!({
            "r": "i", "ts": "2026-03-20T10:00:00Z",
            "t0": "2026-03-20T09:00:00Z", "t1": "2026-03-20T10:00:00Z",
            "sess": "b", "sid": "b", "model": "sonnet", "pk": 1, "hr_et": 10, "dow": 2,
            "input-n": 0, "input-usd": 0, "output-n": 0, "output-usd": 0,
            "r-cache-n": 0, "r-cache-usd": 0, "w-cache-n": 0, "w-cache-usd": 0,
            "w-cache-1h-n": 0, "w-cache-1h-usd": 0, "total-usd": 20.0,
        });

        insert_instance(&conn, &rec1).unwrap();
        insert_instance(&conn, &rec2).unwrap();

        let results = query_instance_compare(&conn, 10).unwrap();
        assert_eq!(results.len(), 2);

        // Should be sorted by total_usd DESC
        assert_eq!(results[0]["sess"], "b");
        assert_eq!(results[0]["total_usd"], 20.0);
        assert_eq!(results[1]["sess"], "a");
        assert_eq!(results[1]["total_usd"], 10.0);
    }

    #[test]
    fn promo_check_view_works() {
        let (_temp, conn) = setup_db();

        let rec = serde_json::json!({
            "r": "i", "ts": "2026-03-20T10:00:00Z",
            "t0": "2026-03-20T09:00:00Z", "t1": "2026-03-20T10:00:00Z",
            "sess": "a", "sid": "a", "model": "sonnet", "pk": 1, "hr_et": 10, "dow": 2,
            "input-n": 0, "input-usd": 0, "output-n": 0, "output-usd": 0,
            "r-cache-n": 0, "r-cache-usd": 0, "w-cache-n": 0, "w-cache-usd": 0,
            "w-cache-1h-n": 0, "w-cache-1h-usd": 0, "total-usd": 5.0,
        });
        insert_instance(&conn, &rec).unwrap();

        let total: f64 = conn
            .query_row(
                "SELECT total_usd FROM promo_check WHERE pk = 1 AND hr_et = 10",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((total - 5.0).abs() < 1e-9);
    }

    #[test]
    fn rebuild_from_jsonl_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let jsonl_path = temp_dir.path().join("history.jsonl");
        let db_path = temp_dir.path().join("history.db");

        // Write test JSONL
        fs::write(
            &jsonl_path,
            r#"{"r":"i","ts":"2026-03-20T10:00:00Z","t0":"2026-03-20T09:55:00Z","t1":"2026-03-20T10:00:00Z","sess":"a","sid":"a","model":"sonnet","pk":1,"hr_et":10,"dow":2,"input-n":100,"input-usd":0.3,"output-n":50,"output-usd":0.75,"r-cache-n":0,"r-cache-usd":0,"w-cache-n":0,"w-cache-usd":0,"w-cache-1h-n":0,"w-cache-1h-usd":0,"total-usd":1.05}
{"r":"f","ts":"2026-03-20T10:00:00Z","t0":"2026-03-20T09:55:00Z","t1":"2026-03-20T10:00:00Z","pk":1,"hr_et":10,"dow":2,"workers":1,"total-usd":1.05,"p75-usd-hr":12.6,"std-usd-hr":0}
{"r":"w","ts":"2026-03-20T10:00:00Z","win":"five_hour","pk":true,"ceil":90.0,"snap":36.0,"reset":"2026-03-20T13:00:00Z","delta":0,"remain":54.0,"hrs_left":3.0,"fleet_pct_hr":2.0,"exh_hrs":27.0,"cutoff_risk":0,"margin_hrs":-24.0,"bind":1,"safe_w":5}
"#,
        )
        .unwrap();

        // First rebuild
        let count1 = rebuild_from_jsonl(&jsonl_path, &db_path).unwrap();
        assert_eq!(count1, 3);

        // Second rebuild (idempotent)
        let count2 = rebuild_from_jsonl(&jsonl_path, &db_path).unwrap();
        assert_eq!(count2, 3);

        // Verify row counts
        let conn = open_db(&db_path).unwrap();
        let i_count: i64 = conn.query_row("SELECT COUNT(*) FROM i", [], |r| r.get(0)).unwrap();
        let f_count: i64 = conn.query_row("SELECT COUNT(*) FROM f", [], |r| r.get(0)).unwrap();
        let w_count: i64 = conn.query_row("SELECT COUNT(*) FROM w", [], |r| r.get(0)).unwrap();
        assert_eq!(i_count, 1);
        assert_eq!(f_count, 1);
        assert_eq!(w_count, 1);
    }

    #[test]
    fn query_last_windows_returns_recent() {
        let (_temp, conn) = setup_db();

        for i in 0..5 {
            let record = serde_json::json!({
                "r": "w", "ts": format!("2026-03-20T{:02}:00:00Z", 10 + i),
                "win": "five_hour", "pk": false, "ceil": 90.0, "snap": 30.0 + i as f64,
                "reset": "2026-03-20T13:00:00Z", "delta": 0.0, "remain": 60.0 - i as f64,
                "hrs_left": 3.0 - i as f64, "fleet_pct_hr": 2.0,
                "exh_hrs": 30.0, "cutoff_risk": 0, "margin_hrs": -27.0,
                "bind": 1, "safe_w": 5,
            });
            insert_window(&conn, &record).unwrap();
        }

        let results = query_last_windows(&conn, 2).unwrap();
        assert_eq!(results.len(), 2);
        // Most recent first
        assert_eq!(results[0]["ts"], "2026-03-20T14:00:00Z");
        assert_eq!(results[1]["ts"], "2026-03-20T13:00:00Z");
    }

    #[test]
    fn query_last_fleets_returns_recent() {
        let (_temp, conn) = setup_db();

        for i in 0..3 {
            let record = serde_json::json!({
                "r": "f", "ts": format!("2026-03-20T{:02}:00:00Z", 10 + i),
                "t0": "2026-03-20T09:55:00Z", "t1": "2026-03-20T10:00:00Z",
                "pk": 1, "hr_et": 10, "dow": 2, "workers": i + 1,
                "total-usd": (i + 1) as f64 * 10.0, "p75-usd-hr": 5.0, "std-usd-hr": 1.0,
            });
            insert_fleet(&conn, &record).unwrap();
        }

        let results = query_last_fleets(&conn, 2).unwrap();
        assert_eq!(results.len(), 2);
    }
}
