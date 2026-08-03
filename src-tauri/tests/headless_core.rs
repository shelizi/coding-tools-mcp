use std::any::TypeId;

use coding_tools_mcp_desktop_lib::core::{
    start_mcp_runtime, AppResult, AppState, DataStore, ExecutionTarget, RuntimeSupervisor,
    WorkspaceProfile,
};

#[test]
fn core_types_are_available_without_a_desktop_host() {
    let _ = TypeId::of::<AppState>();
    let _ = TypeId::of::<DataStore>();
    let _ = TypeId::of::<RuntimeSupervisor>();
    let _ = TypeId::of::<ExecutionTarget>();
}

#[test]
fn core_runtime_entry_point_is_public() {
    fn compile_only(state: &AppState, profile: &WorkspaceProfile) -> AppResult<()> {
        state.with_runtime(|runtime| runtime.start_mcp(profile).map(|_| ()))
    }

    let _ = compile_only as fn(&AppState, &WorkspaceProfile) -> AppResult<()>;

    fn application_service_is_reachable(state: &AppState, profile: &WorkspaceProfile) {
        let _future = start_mcp_runtime(state, &profile.id);
    }

    let _ = application_service_is_reachable as fn(&AppState, &WorkspaceProfile);
}
