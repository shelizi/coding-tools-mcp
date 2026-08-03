//! Async task boundary shared by desktop and future headless hosts.
//!
//! Core modules use this adapter instead of depending on Tauri directly. The
//! desktop feature delegates to Tauri's runtime to preserve existing behavior;
//! headless builds use Tokio and therefore do not require GUI dependencies.

#[cfg(feature = "desktop")]
pub use tauri::async_runtime::{block_on, spawn, JoinHandle};

#[cfg(not(feature = "desktop"))]
mod headless {
    use std::future::Future;
    use std::sync::OnceLock;

    pub use tokio::task::JoinHandle;

    fn runtime() -> &'static tokio::runtime::Runtime {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create the headless Tokio runtime")
        })
    }

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.spawn(future),
            Err(_) => runtime().spawn(future),
        }
    }

    pub fn block_on<F>(future: F) -> F::Output
    where
        F: Future,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
            Err(_) => runtime().block_on(future),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{block_on, spawn};

        #[test]
        fn blocks_without_an_existing_runtime() {
            assert_eq!(block_on(async { 42 }), 42);
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn reuses_an_existing_multithread_runtime() {
            assert_eq!(block_on(async { 21 * 2 }), 42);
            assert_eq!(spawn(async { 6 * 7 }).await.expect("spawned task"), 42);
        }
    }
}

#[cfg(not(feature = "desktop"))]
pub use headless::{block_on, spawn, JoinHandle};
