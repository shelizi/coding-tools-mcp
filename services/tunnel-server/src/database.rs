use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rusqlite::Connection;
use tracing::warn;

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

#[derive(Debug, Clone)]
pub struct DatabaseQueueError(String);

impl DatabaseQueueError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for DatabaseQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DatabaseQueueError {}

struct DatabaseWriterInner {
    sender: Mutex<Option<mpsc::Sender<WriteJob>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for DatabaseWriterInner {
    fn drop(&mut self) {
        if let Ok(sender) = self.sender.get_mut() {
            sender.take();
        }
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

/// A process-wide SQLite writer for the tunnel server.
///
/// All device and worker-policy mutations share this FIFO queue and one connection. Reads that
/// matter to connected workers are served by the stores' in-memory snapshots instead.
#[derive(Clone)]
pub struct DatabaseWriter {
    inner: Arc<DatabaseWriterInner>,
}

impl DatabaseWriter {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, DatabaseQueueError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| DatabaseQueueError::new(error.to_string()))?;
        }

        let (sender, receiver) = mpsc::channel::<WriteJob>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_path = path.clone();
        let worker = thread::Builder::new()
            .name("tunnel-sqlite-writer".into())
            .spawn(move || {
                let connection = open_connection(&worker_path);
                let mut connection = match connection {
                    Ok(connection) => {
                        let _ = ready_tx.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                while let Ok(job) = receiver.recv() {
                    job(&mut connection);
                }
            })
            .map_err(|error| DatabaseQueueError::new(error.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(DatabaseWriterInner {
                    sender: Mutex::new(Some(sender)),
                    worker: Mutex::new(Some(worker)),
                }),
            }),
            Ok(Err(error)) => {
                drop(sender);
                let _ = worker.join();
                Err(DatabaseQueueError::new(error))
            }
            Err(error) => {
                drop(sender);
                let _ = worker.join();
                Err(DatabaseQueueError::new(format!(
                    "SQLite writer failed during startup: {error}"
                )))
            }
        }
    }

    pub fn call<T, F>(&self, operation: F) -> Result<T, DatabaseQueueError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> T + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.send(Box::new(move |connection| {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(connection)))
                    .map_err(|_| DatabaseQueueError::new("SQLite writer job panicked"));
            let _ = reply_tx.send(result);
        }))?;
        reply_rx
            .recv()
            .map_err(|error| DatabaseQueueError::new(format!("SQLite writer stopped: {error}")))?
    }

    pub fn enqueue<F>(&self, operation: F) -> Result<(), DatabaseQueueError>
    where
        F: FnOnce(&mut Connection) -> Result<(), String> + Send + 'static,
    {
        self.send(Box::new(move |connection| {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(connection)));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!(%error, "queued SQLite write failed"),
                Err(_) => warn!("queued SQLite write panicked"),
            }
        }))
    }

    fn send(&self, job: WriteJob) -> Result<(), DatabaseQueueError> {
        let sender = self
            .inner
            .sender
            .lock()
            .map_err(|_| DatabaseQueueError::new("SQLite writer lock poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| DatabaseQueueError::new("SQLite writer is closed"))?;
        sender
            .send(job)
            .map_err(|_| DatabaseQueueError::new("SQLite writer is unavailable"))
    }
}

fn open_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_concurrent_writes_through_one_connection() {
        let directory = tempfile::tempdir().expect("tempdir");
        let writer = DatabaseWriter::open(directory.path().join("queue.db")).expect("writer");
        writer
            .call(|connection| connection.execute("CREATE TABLE counters (value INTEGER)", []))
            .expect("queue")
            .expect("schema");

        let workers = (0..16)
            .map(|_| {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    for value in 0..25_i64 {
                        writer
                            .call(move |connection| {
                                connection
                                    .execute("INSERT INTO counters (value) VALUES (?1)", [value])
                            })
                            .expect("queue")
                            .expect("insert");
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("worker");
        }

        let count = writer
            .call(|connection| {
                connection.query_row("SELECT COUNT(*) FROM counters", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .expect("queue")
            .expect("count");
        assert_eq!(count, 400);
    }
}
