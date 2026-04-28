pub fn log_task_exit(name: &str, result: Result<(), tokio::task::JoinError>) {
    match result {
        Ok(()) => tracing::error!("{name} returned unexpectedly"),
        Err(e) if e.is_panic() => tracing::error!("{name} panicked: {e}"),
        Err(e) if e.is_cancelled() => tracing::debug!("{name} cancelled"),
        Err(e) => tracing::error!("{name} failed: {e}"),
    }
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
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
