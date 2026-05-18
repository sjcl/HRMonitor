use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Spawns a task whose unexpected exit (panic or normal return) must crash the
/// process so the container restarts. Pass `Some(token)` for tasks that return
/// cleanly on graceful shutdown; `None` for tasks expected to run forever.
pub fn spawn_critical_task<F>(
    name: &'static str,
    shutdown: Option<CancellationToken>,
    future: F,
) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(()) if shutdown.as_ref().is_some_and(CancellationToken::is_cancelled) => {}
            Ok(()) => {
                tracing::error!("{name} returned unexpectedly; exiting");
                std::process::exit(1);
            }
            Err(_) => {
                tracing::error!("{name} panicked; exiting");
                std::process::exit(1);
            }
        }
    })
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            biased;
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
