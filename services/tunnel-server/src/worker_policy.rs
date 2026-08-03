use std::fmt;

#[cfg(test)]
use std::path::PathBuf;

use coding_tools_tunnel_protocol::{TunnelService, WorkerPolicy};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};
use tokio::sync::watch;

use crate::database::DatabaseWriter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerPolicyError {
    Invalid(String),
    Storage(String),
}

impl fmt::Display for WorkerPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for WorkerPolicyError {}

impl From<rusqlite::Error> for WorkerPolicyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Clone)]
pub struct WorkerPolicyStore {
    database: DatabaseWriter,
    mcp: watch::Sender<WorkerPolicy>,
    actions: watch::Sender<WorkerPolicy>,
}

impl WorkerPolicyStore {
    #[cfg(test)]
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, WorkerPolicyError> {
        let database = DatabaseWriter::open(path.into())
            .map_err(|error| WorkerPolicyError::Storage(error.to_string()))?;
        Self::from_writer(database)
    }

    pub fn from_writer(database: DatabaseWriter) -> Result<Self, WorkerPolicyError> {
        let (mcp, actions) = database
            .call(|connection| {
                initialize_schema(connection)?;
                Ok::<_, WorkerPolicyError>((
                    load_policy(connection, TunnelService::Mcp)?,
                    load_policy(connection, TunnelService::Actions)?,
                ))
            })
            .map_err(|error| WorkerPolicyError::Storage(error.to_string()))??;
        let (mcp, _) = watch::channel(mcp);
        let (actions, _) = watch::channel(actions);
        Ok(Self {
            database,
            mcp,
            actions,
        })
    }

    pub fn current(&self, service: TunnelService) -> WorkerPolicy {
        self.sender(service).borrow().clone()
    }

    pub fn subscribe(&self, service: TunnelService) -> watch::Receiver<WorkerPolicy> {
        self.sender(service).subscribe()
    }

    pub fn update(
        &self,
        service: TunnelService,
        mut policy: WorkerPolicy,
    ) -> Result<WorkerPolicy, WorkerPolicyError> {
        let saved = self
            .database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let revision = transaction
                    .query_row(
                        "SELECT revision FROM worker_policies WHERE service = ?1",
                        params![service.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    .max(0) as u64;
                policy.revision = revision.saturating_add(1);
                policy.validate().map_err(WorkerPolicyError::Invalid)?;
                transaction.execute(
                    "INSERT INTO worker_policies
                     (service, start_workers, min_idle_workers, max_idle_workers, max_workers,
                      max_requests_per_worker, max_lifetime_seconds, scale_down_delay_seconds,
                      recycle_jitter_percent, revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(service) DO UPDATE SET
                       start_workers = excluded.start_workers,
                       min_idle_workers = excluded.min_idle_workers,
                       max_idle_workers = excluded.max_idle_workers,
                       max_workers = excluded.max_workers,
                       max_requests_per_worker = excluded.max_requests_per_worker,
                       max_lifetime_seconds = excluded.max_lifetime_seconds,
                       scale_down_delay_seconds = excluded.scale_down_delay_seconds,
                       recycle_jitter_percent = excluded.recycle_jitter_percent,
                       revision = excluded.revision",
                    params_from_iter(policy_values(service, &policy)),
                )?;
                transaction.commit()?;
                Ok::<_, WorkerPolicyError>(policy)
            })
            .map_err(|error| WorkerPolicyError::Storage(error.to_string()))??;
        self.sender(service).send_replace(saved.clone());
        Ok(saved)
    }

    fn sender(&self, service: TunnelService) -> &watch::Sender<WorkerPolicy> {
        match service {
            TunnelService::Mcp => &self.mcp,
            TunnelService::Actions => &self.actions,
        }
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), WorkerPolicyError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS worker_policies (
            service TEXT PRIMARY KEY,
            start_workers INTEGER NOT NULL,
            min_idle_workers INTEGER NOT NULL,
            max_idle_workers INTEGER NOT NULL,
            max_workers INTEGER NOT NULL,
            max_requests_per_worker INTEGER NOT NULL,
            max_lifetime_seconds INTEGER NOT NULL,
            scale_down_delay_seconds INTEGER NOT NULL,
            recycle_jitter_percent INTEGER NOT NULL,
            revision INTEGER NOT NULL
         );",
    )?;
    for service in [TunnelService::Mcp, TunnelService::Actions] {
        let policy = WorkerPolicy::default_for(service);
        connection.execute(
            "INSERT OR IGNORE INTO worker_policies
             (service, start_workers, min_idle_workers, max_idle_workers, max_workers,
              max_requests_per_worker, max_lifetime_seconds, scale_down_delay_seconds,
              recycle_jitter_percent, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params_from_iter(policy_values(service, &policy)),
        )?;
    }
    Ok(())
}

fn policy_values(service: TunnelService, policy: &WorkerPolicy) -> [Value; 10] {
    [
        Value::Text(service.as_str().to_string()),
        Value::Integer(i64::from(policy.start_workers)),
        Value::Integer(i64::from(policy.min_idle_workers)),
        Value::Integer(i64::from(policy.max_idle_workers)),
        Value::Integer(i64::from(policy.max_workers)),
        Value::Integer(policy.max_requests_per_worker as i64),
        Value::Integer(policy.max_lifetime_seconds as i64),
        Value::Integer(policy.scale_down_delay_seconds as i64),
        Value::Integer(i64::from(policy.recycle_jitter_percent)),
        Value::Integer(policy.revision as i64),
    ]
}

fn load_policy(
    connection: &Connection,
    service: TunnelService,
) -> Result<WorkerPolicy, WorkerPolicyError> {
    let policy = connection
        .query_row(
            "SELECT start_workers, min_idle_workers, max_idle_workers, max_workers,
                    max_requests_per_worker, max_lifetime_seconds,
                    scale_down_delay_seconds, recycle_jitter_percent, revision
             FROM worker_policies WHERE service = ?1",
            params![service.as_str()],
            |row| {
                Ok(WorkerPolicy {
                    start_workers: row.get(0)?,
                    min_idle_workers: row.get(1)?,
                    max_idle_workers: row.get(2)?,
                    max_workers: row.get(3)?,
                    max_requests_per_worker: row.get(4)?,
                    max_lifetime_seconds: row.get(5)?,
                    scale_down_delay_seconds: row.get(6)?,
                    recycle_jitter_percent: row.get(7)?,
                    revision: row.get(8)?,
                })
            },
        )
        .map_err(WorkerPolicyError::from)?;
    policy.validate().map_err(WorkerPolicyError::Invalid)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use coding_tools_tunnel_protocol::{TunnelService, WorkerPolicy};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn opens_with_independent_default_policies_and_persists_updates() {
        let directory = tempdir().expect("policy tempdir");
        let path = directory.path().join("tunnel.db");
        let store = WorkerPolicyStore::open(&path).expect("policy store");

        assert_eq!(
            store.current(TunnelService::Mcp),
            WorkerPolicy::default_for(TunnelService::Mcp)
        );
        assert_eq!(
            store.current(TunnelService::Actions),
            WorkerPolicy::default_for(TunnelService::Actions)
        );

        let mut changed = store.current(TunnelService::Mcp);
        changed.start_workers = 6;
        changed.max_idle_workers = 8;
        changed.max_workers = 24;
        let saved = store
            .update(TunnelService::Mcp, changed)
            .expect("update policy");
        assert_eq!(saved.revision, 2);

        let reopened = WorkerPolicyStore::open(&path).expect("reopen policy store");
        assert_eq!(reopened.current(TunnelService::Mcp), saved);
        assert_eq!(
            reopened.current(TunnelService::Actions),
            WorkerPolicy::default_for(TunnelService::Actions)
        );
    }

    #[test]
    fn broadcasts_saved_revisions_and_rejects_invalid_policy() {
        let directory = tempdir().expect("policy tempdir");
        let store =
            WorkerPolicyStore::open(directory.path().join("tunnel.db")).expect("policy store");
        let mut updates = store.subscribe(TunnelService::Actions);

        let mut changed = store.current(TunnelService::Actions);
        changed.max_requests_per_worker = 900;
        let saved = store
            .update(TunnelService::Actions, changed)
            .expect("save policy");
        assert!(updates.has_changed().expect("watch policy"));
        assert_eq!(*updates.borrow_and_update(), saved);

        let mut invalid = saved.clone();
        invalid.min_idle_workers = invalid.max_workers + 1;
        assert!(matches!(
            store.update(TunnelService::Actions, invalid),
            Err(WorkerPolicyError::Invalid(_))
        ));
        assert_eq!(store.current(TunnelService::Actions), saved);
    }

    #[test]
    fn rejects_corrupt_persisted_policy_values() {
        let directory = tempdir().expect("policy tempdir");
        let path = directory.path().join("tunnel.db");
        let store = WorkerPolicyStore::open(&path).expect("initialize policy store");
        let database = store.database.clone();
        database
            .call(|connection| {
                connection.execute(
                    "UPDATE worker_policies SET min_idle_workers = -1 WHERE service = 'mcp'",
                    [],
                )
            })
            .expect("queue")
            .expect("corrupt policy");
        drop(store);

        assert!(WorkerPolicyStore::open(&path).is_err());
    }
}
