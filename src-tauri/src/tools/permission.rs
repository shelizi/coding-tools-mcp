use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::tools::workspace::WorkspaceError;

const MAX_PENDING_OPERATIONS: usize = 256;

#[derive(Clone, Debug)]
pub struct PendingOperation {
    pub resume_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub permission: String,
    pub reason: String,
    pub expires_at: Instant,
}

#[derive(Default)]
pub struct PendingOperationStore {
    operations: Mutex<HashMap<String, PendingOperation>>,
}

impl PendingOperationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &self,
        tool_name: &str,
        arguments: &Value,
        permission: &str,
        reason: &str,
        ttl: Duration,
    ) -> PendingOperation {
        self.remove_expired();
        let operation = PendingOperation {
            resume_id: Uuid::new_v4().to_string(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            permission: permission.to_string(),
            reason: reason.to_string(),
            expires_at: Instant::now() + ttl,
        };
        let mut operations = self.operations.lock().expect("pending operations lock");
        if operations.len() >= MAX_PENDING_OPERATIONS {
            if let Some(oldest) = operations
                .values()
                .min_by_key(|pending| pending.expires_at)
                .map(|pending| pending.resume_id.clone())
            {
                operations.remove(&oldest);
            }
        }
        operations.insert(operation.resume_id.clone(), operation.clone());
        operation
    }

    pub fn take(&self, resume_id: &str) -> Result<PendingOperation, WorkspaceError> {
        self.remove_expired();
        self.operations
            .lock()
            .expect("pending operations lock")
            .remove(resume_id)
            .ok_or_else(|| WorkspaceError::Tool {
                code: "RESUME_OPERATION_NOT_FOUND",
                message: format!("Pending operation not found or expired: {resume_id}"),
                category: "not_found",
                retryable: false,
            })
    }

    pub fn tool_name(&self, resume_id: &str) -> Option<String> {
        self.remove_expired();
        self.operations
            .lock()
            .expect("pending operations lock")
            .get(resume_id)
            .map(|operation| operation.tool_name.clone())
    }

    pub fn contains(&self, resume_id: &str) -> bool {
        self.remove_expired();
        self.operations
            .lock()
            .expect("pending operations lock")
            .contains_key(resume_id)
    }

    pub fn put_back(&self, operation: PendingOperation) {
        if operation.expires_at > Instant::now() {
            self.operations
                .lock()
                .expect("pending operations lock")
                .insert(operation.resume_id.clone(), operation);
        }
    }

    fn remove_expired(&self) {
        let now = Instant::now();
        self.operations
            .lock()
            .expect("pending operations lock")
            .retain(|_, operation| operation.expires_at > now);
    }
}
