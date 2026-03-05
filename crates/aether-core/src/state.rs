use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Error as SqlError};
use sha2::{Digest, Sha256};

use crate::types::{EventRecord, PendingApproval, PersistedRun, RunStatus};

#[derive(Clone)]
pub struct StateStore {
    conn: Arc<Mutex<Connection>>,
}

impl StateStore {
    pub fn new(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init()?;
        Ok(store)
    }

    pub fn init(&self) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                workflow TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                total_tokens INTEGER NOT NULL,
                total_cost_usd REAL NOT NULL,
                step_count INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS run_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                prev_hash TEXT,
                event_hash TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS approvals (
                run_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                action TEXT NOT NULL,
                threshold_usd REAL NOT NULL,
                status TEXT NOT NULL,
                approved_by TEXT,
                approved_reason TEXT,
                approved_at TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (run_id, step_id)
            );
            CREATE TABLE IF NOT EXISTS memory_nodes (
                node_id TEXT PRIMARY KEY,
                namespace TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                vector_ref TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_edges (
                edge_id INTEGER PRIMARY KEY AUTOINCREMENT,
                src_node_id TEXT NOT NULL,
                dst_node_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            ",
        )?;
        add_column_if_missing(&conn, "approvals", "approved_reason", "TEXT")?;
        Ok(())
    }

    pub fn create_run(&self, run_id: &str, workflow: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        conn.execute(
            "
            INSERT INTO runs(run_id, workflow, status, created_at, updated_at, total_tokens, total_cost_usd, step_count)
            VALUES (?1, ?2, ?3, ?4, ?5, 0, 0.0, 0)
            ",
            params![run_id, workflow, "running", now, now],
        )?;
        Ok(())
    }

    pub fn update_run(
        &self,
        run_id: &str,
        status: RunStatus,
        total_tokens: u64,
        total_cost_usd: f64,
        step_count: u32,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        conn.execute(
            "
            UPDATE runs
            SET status = ?2, updated_at = ?3, total_tokens = ?4, total_cost_usd = ?5, step_count = ?6
            WHERE run_id = ?1
            ",
            params![
                run_id,
                status_to_str(&status),
                now,
                total_tokens as i64,
                total_cost_usd,
                step_count as i64
            ],
        )?;
        Ok(())
    }

    pub fn append_event(
        &self,
        run_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let payload_json = serde_json::to_string(payload)?;
        let created_at = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let prev_hash: Option<String> = conn
            .query_row(
                "SELECT event_hash FROM run_events WHERE run_id=?1 ORDER BY seq DESC LIMIT 1",
                params![run_id],
                |row| row.get(0),
            )
            .ok();
        let event_hash = chain_hash(
            prev_hash.as_deref(),
            run_id,
            event_type,
            &payload_json,
            &created_at,
        );
        conn.execute(
            "
            INSERT INTO run_events(run_id, event_type, payload, created_at, prev_hash, event_hash)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                run_id,
                event_type,
                payload_json,
                created_at,
                prev_hash,
                event_hash
            ],
        )?;
        Ok(())
    }

    pub fn create_approval_request(
        &self,
        run_id: &str,
        step_id: &str,
        action: &str,
        threshold_usd: f64,
    ) -> anyhow::Result<()> {
        let created_at = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        conn.execute(
            "
            INSERT OR REPLACE INTO approvals(run_id, step_id, action, threshold_usd, status, created_at)
            VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
            ",
            params![run_id, step_id, action, threshold_usd, created_at],
        )?;
        Ok(())
    }

    pub fn approve(
        &self,
        run_id: &str,
        step_id: &str,
        actor: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        let approved_at = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let changed = conn.execute(
            "
            UPDATE approvals
            SET status='approved', approved_by=?3, approved_reason=?4, approved_at=?5
            WHERE run_id=?1 AND step_id=?2 AND status='pending'
            ",
            params![run_id, step_id, actor, reason, approved_at],
        )?;
        Ok(changed > 0)
    }

    pub fn is_approved(&self, run_id: &str, step_id: &str) -> anyhow::Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM approvals WHERE run_id=?1 AND step_id=?2",
                params![run_id, step_id],
                |row| row.get(0),
            )
            .ok();
        Ok(matches!(status.as_deref(), Some("approved")))
    }

    pub fn get_run(&self, run_id: &str) -> anyhow::Result<Option<PersistedRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let mut stmt = conn.prepare(
            "
            SELECT run_id, workflow, status, created_at, updated_at, total_tokens, total_cost_usd, step_count
            FROM runs WHERE run_id = ?1
            ",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        if let Some(row) = rows.next()? {
            let status: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            let updated_at: String = row.get(4)?;
            return Ok(Some(PersistedRun {
                run_id: row.get(0)?,
                workflow: row.get(1)?,
                status: str_to_status(&status),
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
                total_tokens: row.get::<_, i64>(5)? as u64,
                total_cost_usd: row.get(6)?,
                step_count: row.get::<_, i64>(7)? as u32,
            }));
        }
        Ok(None)
    }

    pub fn list_events(&self, run_id: &str) -> anyhow::Result<Vec<EventRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let mut stmt = conn.prepare(
            "
            SELECT seq, run_id, event_type, payload, created_at, prev_hash, event_hash
            FROM run_events
            WHERE run_id=?1
            ORDER BY seq ASC
            ",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            events.push(EventRecord {
                seq: row.get(0)?,
                run_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: serde_json::from_str(&payload)?,
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
                prev_hash: row.get(5)?,
                event_hash: row.get(6)?,
            });
        }
        Ok(events)
    }

    pub fn list_runs(&self, limit: usize) -> anyhow::Result<Vec<PersistedRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let mut stmt = conn.prepare(
            "
            SELECT run_id, workflow, status, created_at, updated_at, total_tokens, total_cost_usd, step_count
            FROM runs
            ORDER BY created_at DESC
            LIMIT ?1
            ",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            let status: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            let updated_at: String = row.get(4)?;
            runs.push(PersistedRun {
                run_id: row.get(0)?,
                workflow: row.get(1)?,
                status: str_to_status(&status),
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
                updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
                total_tokens: row.get::<_, i64>(5)? as u64,
                total_cost_usd: row.get(6)?,
                step_count: row.get::<_, i64>(7)? as u32,
            });
        }
        Ok(runs)
    }

    pub fn list_pending_approvals(&self, limit: usize) -> anyhow::Result<Vec<PendingApproval>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let mut stmt = conn.prepare(
            "
            SELECT run_id, step_id, action, threshold_usd, status, created_at
            FROM approvals
            WHERE status = 'pending'
            ORDER BY created_at ASC
            LIMIT ?1
            ",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        let mut approvals = Vec::new();
        while let Some(row) = rows.next()? {
            let created_at: String = row.get(5)?;
            approvals.push(PendingApproval {
                run_id: row.get(0)?,
                step_id: row.get(1)?,
                action: row.get(2)?,
                threshold_usd: row.get(3)?,
                status: row.get(4)?,
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
            });
        }
        Ok(approvals)
    }

    pub fn tokens_used_today(&self) -> anyhow::Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
        let today = Utc::now().date_naive().to_string();
        let mut stmt = conn.prepare(
            "
            SELECT COALESCE(SUM(total_tokens), 0)
            FROM runs
            WHERE substr(created_at, 1, 10) = ?1
            ",
        )?;
        let total: i64 = stmt.query_row(params![today], |row| row.get(0))?;
        Ok(total as u64)
    }

    pub fn verify_chain(&self, run_id: &str) -> anyhow::Result<bool> {
        let events = self.list_events(run_id)?;
        let mut prev_hash: Option<String> = None;
        for event in events {
            let payload_json = serde_json::to_string(&event.payload)?;
            let recomputed = chain_hash(
                prev_hash.as_deref(),
                &event.run_id,
                &event.event_type,
                &payload_json,
                &event.created_at.to_rfc3339(),
            );
            if recomputed != event.event_hash {
                return Ok(false);
            }
            prev_hash = Some(event.event_hash);
        }
        Ok(true)
    }
}

fn chain_hash(
    prev_hash: Option<&str>,
    run_id: &str,
    event_type: &str,
    payload: &str,
    created_at: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.unwrap_or_default());
    hasher.update(run_id);
    hasher.update(event_type);
    hasher.update(payload);
    hasher.update(created_at);
    format!("{:x}", hasher.finalize())
}

fn status_to_str(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::WaitingApproval => "waiting_approval",
        RunStatus::BudgetExceeded => "budget_exceeded",
        RunStatus::Killed => "killed",
    }
}

fn str_to_status(value: &str) -> RunStatus {
    match value {
        "running" => RunStatus::Running,
        "succeeded" => RunStatus::Succeeded,
        "failed" => RunStatus::Failed,
        "waiting_approval" => RunStatus::WaitingApproval,
        "budget_exceeded" => RunStatus::BudgetExceeded,
        "killed" => RunStatus::Killed,
        _ => RunStatus::Failed,
    }
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> anyhow::Result<()> {
    let query = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
    match conn.execute(&query, []) {
        Ok(_) => Ok(()),
        Err(err) if is_duplicate_column_error(&err) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn is_duplicate_column_error(err: &SqlError) -> bool {
    matches!(
        err,
        SqlError::SqliteFailure(_, Some(message))
            if message.to_ascii_lowercase().contains("duplicate column name")
    )
}
