//! Embedded SQLite baseline store.

use crate::error::{Error, Result};
use crate::l7::L7Metrics;
use crate::meta::RunMetadata;
use crate::traceroute::TraceResult;
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS targets (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    endpoint    TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id          TEXT PRIMARY KEY,
    target_id   INTEGER NOT NULL REFERENCES targets(id),
    created_at  TEXT NOT NULL,
    resolved_ip TEXT,
    reached     INTEGER NOT NULL DEFAULT 0,
    hop_json    TEXT,
    l7_json     TEXT,
    meta_json   TEXT
);

CREATE TABLE IF NOT EXISTS baselines (
    name        TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS hop_metrics (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    ttl         INTEGER NOT NULL,
    address     TEXT,
    loss_pct    REAL,
    min_ms      REAL,
    avg_ms      REAL,
    p50_ms      REAL,
    p95_ms      REAL,
    jitter_ms   REAL,
    sent        INTEGER,
    recv        INTEGER
);

CREATE TABLE IF NOT EXISTS l7_metrics (
    run_id      TEXT PRIMARY KEY REFERENCES runs(id),
    url         TEXT,
    status      INTEGER,
    dns_ms      REAL,
    tcp_ms      REAL,
    tls_ms      REAL,
    ttfb_ms     REAL,
    transfer_ms REAL,
    total_ms    REAL,
    bytes_read  INTEGER
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRun {
    pub id: String,
    pub target: String,
    pub created_at: DateTime<Utc>,
    pub resolved_ip: Option<String>,
    pub reached: bool,
    pub trace: Option<TraceResult>,
    pub l7: Option<L7Metrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RunMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineInfo {
    pub name: String,
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub target: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let db_path = match path {
            Some(p) => p.to_path_buf(),
            None => default_db_path()?,
        };
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(SCHEMA)?;
        // Migrate older DBs that lack meta_json.
        let _ = conn.execute("ALTER TABLE runs ADD COLUMN meta_json TEXT", []);
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn save_run(
        &self,
        target: &str,
        trace: Option<&TraceResult>,
        l7: Option<&L7Metrics>,
    ) -> Result<String> {
        self.save_run_with_meta(target, trace, l7, None)
    }

    pub fn save_run_with_meta(
        &self,
        target: &str,
        trace: Option<&TraceResult>,
        l7: Option<&L7Metrics>,
        meta: Option<&RunMetadata>,
    ) -> Result<String> {
        let run_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT OR IGNORE INTO targets (endpoint, created_at) VALUES (?1, ?2)",
            params![target, now],
        )?;
        let target_id: i64 = self.conn.query_row(
            "SELECT id FROM targets WHERE endpoint = ?1",
            params![target],
            |r| r.get(0),
        )?;

        let resolved = trace
            .map(|t| t.resolved.to_string())
            .or_else(|| l7.and_then(|m| m.resolved_ip.clone()));
        let reached = trace.map(|t| t.reached).unwrap_or(false);
        let hop_json = trace.map(serde_json::to_string).transpose()?;
        let l7_json = l7.map(serde_json::to_string).transpose()?;
        let meta_json = meta.map(serde_json::to_string).transpose()?;

        self.conn.execute(
            "INSERT INTO runs (id, target_id, created_at, resolved_ip, reached, hop_json, l7_json, meta_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                target_id,
                now,
                resolved,
                reached as i32,
                hop_json,
                l7_json,
                meta_json
            ],
        )?;

        if let Some(tr) = trace {
            for hop in &tr.hops {
                self.conn.execute(
                    "INSERT INTO hop_metrics
                     (run_id, ttl, address, loss_pct, min_ms, avg_ms, p50_ms, p95_ms, jitter_ms, sent, recv)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        run_id,
                        hop.ttl as i32,
                        hop.address.map(|a| a.to_string()),
                        hop.metrics.loss_pct,
                        hop.metrics.min_ms,
                        hop.metrics.avg_ms,
                        hop.metrics.p50_ms,
                        hop.metrics.p95_ms,
                        hop.metrics.jitter_ms,
                        hop.metrics.sent as i32,
                        hop.metrics.recv as i32,
                    ],
                )?;
            }
        }

        if let Some(m) = l7 {
            self.conn.execute(
                "INSERT INTO l7_metrics
                 (run_id, url, status, dns_ms, tcp_ms, tls_ms, ttfb_ms, transfer_ms, total_ms, bytes_read)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    run_id,
                    m.url,
                    m.status.map(|s| s as i32),
                    m.dns_ms,
                    m.tcp_ms,
                    m.tls_ms,
                    m.ttfb_ms,
                    m.transfer_ms,
                    m.total_ms,
                    m.bytes_read as i64,
                ],
            )?;
        }

        Ok(run_id)
    }

    pub fn tag_baseline(&self, run_id: &str, name: &str) -> Result<()> {
        let exists: bool = self.conn.query_row(
            "SELECT COUNT(1) FROM runs WHERE id = ?1",
            params![run_id],
            |r| r.get::<_, i64>(0).map(|c| c > 0),
        )?;
        if !exists {
            return Err(Error::Other(format!("run '{run_id}' not found")));
        }
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO baselines (name, run_id, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET run_id = excluded.run_id, created_at = excluded.created_at",
            params![name, run_id, now],
        )?;
        Ok(())
    }

    pub fn delete_baseline(&self, name: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM baselines WHERE name = ?1", params![name])?;
        if n == 0 {
            return Err(Error::BaselineNotFound(name.to_string()));
        }
        Ok(())
    }

    pub fn get_baseline(&self, name: &str) -> Result<StoredRun> {
        let run_id: String = self
            .conn
            .query_row(
                "SELECT run_id FROM baselines WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::BaselineNotFound(name.to_string()))?;
        self.get_run(&run_id)
    }

    pub fn get_run(&self, run_id: &str) -> Result<StoredRun> {
        self.conn
            .query_row(
                "SELECT r.id, t.endpoint, r.created_at, r.resolved_ip, r.reached, r.hop_json, r.l7_json, r.meta_json
                 FROM runs r JOIN targets t ON t.id = r.target_id WHERE r.id = ?1",
                params![run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i32>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?
            .map(|(id, target, created, resolved, reached, hop_json, l7_json, meta_json)| {
                let created_at = DateTime::parse_from_rfc3339(&created)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok(StoredRun {
                    id,
                    target,
                    created_at,
                    resolved_ip: resolved,
                    reached: reached != 0,
                    trace: hop_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                    l7: l7_json.as_deref().map(serde_json::from_str).transpose()?,
                    meta: meta_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                })
            })
            .ok_or_else(|| Error::Other(format!("run '{run_id}' not found")))?
    }

    pub fn latest_run(&self) -> Result<Option<StoredRun>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM runs ORDER BY created_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => Ok(Some(self.get_run(&id)?)),
            None => Ok(None),
        }
    }

    pub fn list_baselines(&self) -> Result<Vec<BaselineInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.name, b.run_id, b.created_at, t.endpoint
             FROM baselines b
             JOIN runs r ON r.id = b.run_id
             JOIN targets t ON t.id = r.target_id
             ORDER BY b.created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(name, run_id, created, target)| BaselineInfo {
                name,
                run_id,
                created_at: DateTime::parse_from_rfc3339(&created)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                target,
            })
            .collect())
    }

    pub fn list_runs(&self, limit: usize) -> Result<Vec<StoredRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM runs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![limit as i64], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| self.get_run(&id)).collect()
    }
}

pub fn default_db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "trace-diff", "trace-diff").ok_or_else(|| {
        Error::Other("could not resolve platform data directory".into())
    })?;
    Ok(dirs.data_dir().join("trace-diff.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::LatencySummary;

    #[test]
    fn save_and_baseline() {
        let store = Store::open_in_memory().unwrap();
        let l7 = L7Metrics {
            url: "https://example.com".into(),
            resolved_ip: Some("93.184.216.34".into()),
            status: Some(200),
            dns_ms: Some(12.0),
            tcp_ms: Some(30.0),
            tls_ms: Some(45.0),
            ttfb_ms: Some(80.0),
            transfer_ms: Some(10.0),
            total_ms: 177.0,
            bytes_read: 1256,
            measured_at: Utc::now(),
        };
        let id = store.save_run("https://example.com", None, Some(&l7)).unwrap();
        store.tag_baseline(&id, "staging").unwrap();
        let b = store.get_baseline("staging").unwrap();
        assert_eq!(b.l7.as_ref().unwrap().ttfb_ms, Some(80.0));
        assert!(store.list_baselines().unwrap().iter().any(|x| x.name == "staging"));
    }

    #[test]
    fn hop_metrics_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let trace = TraceResult {
            target: "1.1.1.1".into(),
            resolved: "1.1.1.1".parse().unwrap(),
            reached: true,
            probe_kind: Default::default(),
            dest_port: None,
            summary: Default::default(),
            hops: vec![crate::traceroute::HopResult {
                ttl: 1,
                address: Some("1.1.1.1".parse().unwrap()),
                hostname: None,
                asn: None,
                as_name: None,
                reply_proto: Some(crate::traceroute::ReplyProto::Icmp),
                protos_seen: vec![crate::traceroute::ReplyProto::Icmp],
                metrics: LatencySummary::from_samples(&[10.0, 12.0, 11.0], 3),
                samples_ms: vec![10.0, 12.0, 11.0],
            }],
        };
        let id = store.save_run("1.1.1.1", Some(&trace), None).unwrap();
        let run = store.get_run(&id).unwrap();
        assert_eq!(run.trace.unwrap().hops.len(), 1);
    }
}
