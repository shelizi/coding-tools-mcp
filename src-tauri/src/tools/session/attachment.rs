use std::sync::atomic::Ordering;

use crate::harness::{model::OperationRecord, Harness};

use super::{lifecycle, ExecSession, HarnessOperationTracking};

impl ExecSession {
    pub fn attach_harness_operation(&self, harness: Harness, operation: OperationRecord) {
        let mut operations = self
            .harness_operations
            .lock()
            .expect("harness operation lock");
        if let Some(existing) = operations
            .iter_mut()
            .find(|tracking| tracking.operation.id == operation.id)
        {
            *existing = HarnessOperationTracking { harness, operation };
        } else {
            operations.push(HarnessOperationTracking { harness, operation });
        }
        drop(operations);
        if self.is_finalized() {
            lifecycle::record_harness_operation_finalization(self);
        }
    }

    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    pub fn command_fingerprint(&self) -> Option<&str> {
        self.command_fingerprint.as_deref()
    }

    pub fn touch_attachment(&self) {
        self.attachment_generation.fetch_add(1, Ordering::AcqRel);
        self.detached_generation.store(0, Ordering::Release);
    }

    pub fn mark_detached(&self) -> u64 {
        let generation = self.attachment_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.detached_generation
            .store(generation, Ordering::Release);
        generation
    }

    pub fn is_still_detached(&self, generation: u64) -> bool {
        generation != 0
            && self.detached_generation.load(Ordering::Acquire) == generation
            && self.attachment_generation.load(Ordering::Acquire) == generation
    }
}
