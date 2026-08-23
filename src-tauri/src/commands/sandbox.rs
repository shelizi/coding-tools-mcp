use crate::tools::sandbox::{backend_descriptors, SandboxBackendDescriptor};

#[tauri::command]
pub fn list_sandbox_backends() -> Vec<SandboxBackendDescriptor> {
    backend_descriptors()
}
