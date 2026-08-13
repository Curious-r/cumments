//! The reconcile-pass contract and the per-pass task runner.
//!
//! Each pass is an independent controller over one kind of local work. It is
//! event-driven (its own wakeup channel) with a fixed resync interval as the
//! level-triggered fallback, mirroring the Kubernetes controller shape.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{error, info};

/// Scheduling identity of one pass.
#[derive(Clone)]
pub struct PassConfig {
    pub name: &'static str,
    /// Fallback resync interval; wakeups run the pass before this elapses.
    pub interval: Duration,
    /// The channel this pass listens on.
    pub wakeup: Arc<Notify>,
}

/// A background controller that reconciles one kind of work toward Matrix.
#[async_trait]
pub trait ReconcilePass: Send + Sync {
    fn config(&self) -> &PassConfig;

    /// Runs one reconciliation sweep, returning how many work items it
    /// processed. Failures are logged per item inside the pass and the error
    /// is returned so the runner can report the pass as failed.
    async fn run(&self) -> Result<u64>;
}

/// The task body every pass runs: wait for either its wakeup or its resync
/// interval, then sweep once. A failing or slow pass never affects others.
pub async fn run_pass(pass: Arc<dyn ReconcilePass>) {
    let mut interval = tokio::time::interval(pass.config().interval);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = pass.config().wakeup.notified() => {}
        }
        match pass.run().await {
            Ok(processed) if processed > 0 => {
                info!(
                    pass = pass.config().name,
                    processed, "reconcile pass finished"
                );
            }
            Ok(_) => {}
            Err(error) => {
                error!(
                    pass = pass.config().name,
                    %error,
                    "reconcile pass failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingPass {
        config: PassConfig,
        runs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ReconcilePass for CountingPass {
        fn config(&self) -> &PassConfig {
            &self.config
        }

        async fn run(&self) -> Result<u64> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }
    }

    #[tokio::test]
    async fn run_pass_ignores_unrelated_wakeups() {
        let wakeup = Arc::new(Notify::new());
        let other = Arc::new(Notify::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let pass = Arc::new(CountingPass {
            config: PassConfig {
                name: "counting",
                interval: Duration::from_secs(3600),
                wakeup: wakeup.clone(),
            },
            runs: runs.clone(),
        });
        tokio::spawn(run_pass(pass));

        // The interval ticks once immediately; wait for that baseline run.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        other.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "an unrelated wakeup must not run the pass"
        );

        wakeup.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "the pass's own wakeup must run it"
        );
    }
}
