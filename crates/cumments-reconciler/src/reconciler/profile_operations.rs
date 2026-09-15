//! Profile-operation reconciliation: executes durable pending profile mutations.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::ports::ProfileStore;
use cumments_core::profile::{ProfileOperationExecutionResult, ProfileOperationExecutor};
use tracing::{error, info, warn};

/// Batch size of executable profile operations claimed per sweep.
const PROFILE_OP_BATCH_SIZE: u64 = 50;

/// Reconciles durable pending profile operations toward Matrix homeserver.
pub struct ProfileOperationsPass {
    executor: ProfileOperationExecutor,
    store: Arc<dyn ProfileStore>,
    config: PassConfig,
}

impl ProfileOperationsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        let profile_store = deps
            .profile_store
            .as_ref()
            .expect("profile_store must be configured for ProfileOperationsPass")
            .clone();
        let executor = ProfileOperationExecutor::new(profile_store.clone(), deps.driver.clone());
        Self {
            executor,
            store: profile_store,
            config,
        }
    }

    /// Constructs a pass with explicit dependencies, useful for standalone tests and workers.
    pub fn with_deps(
        store: Arc<dyn ProfileStore>,
        driver: Arc<dyn cumments_core::ports::MatrixDriver>,
        config: PassConfig,
    ) -> Self {
        let executor = ProfileOperationExecutor::new(store.clone(), driver);
        Self {
            executor,
            store,
            config,
        }
    }

    /// Convenience constructor for tests with default configuration.
    pub fn for_test(
        store: Arc<dyn ProfileStore>,
        driver: Arc<dyn cumments_core::ports::MatrixDriver>,
    ) -> Self {
        Self::with_deps(
            store,
            driver,
            PassConfig {
                name: "profile_operations",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(tokio::sync::Notify::new()),
            },
        )
    }

    /// Reconciles executable pending profile operations.
    ///
    /// Repeatedly finds earliest executable `Pending` operations across all visitors
    /// and fields, executes them through `ProfileOperationExecutor`, and
    /// loops so that next same-field `Pending` operations become eligible and execute
    /// in the same pass. Operations blocked by `Dispatching` or `Unknown` are skipped.
    pub async fn reconcile(&self) -> Result<u64> {
        let mut total_handled = 0u64;

        loop {
            let batch = self
                .store
                .list_executable_pending_operations(PROFILE_OP_BATCH_SIZE)
                .await?;

            if batch.is_empty() {
                break;
            }

            let mut executed_any = false;

            for op in batch {
                let res = self.executor.execute(&op.operation_id).await;
                match res {
                    Ok(ProfileOperationExecutionResult::Completed)
                    | Ok(ProfileOperationExecutionResult::Failed(_)) => {
                        total_handled += 1;
                        executed_any = true;
                    }
                    Ok(ProfileOperationExecutionResult::Unknown(err)) => {
                        warn!(
                            operation_id = %op.operation_id,
                            %err,
                            "profile operation entered unknown state and will block further operations for this field"
                        );
                        total_handled += 1;
                        executed_any = true;
                    }
                    Ok(ProfileOperationExecutionResult::AlreadyProcessed(_)) => {
                        total_handled += 1;
                        executed_any = true;
                    }
                    Ok(ProfileOperationExecutionResult::Blocked(reason)) => {
                        // Concurrent execution claim or blocked dependency
                        info!(
                            operation_id = %op.operation_id,
                            %reason,
                            "profile operation blocked from execution"
                        );
                    }
                    Err(e) => {
                        error!(
                            operation_id = %op.operation_id,
                            error = %e,
                            "unexpected error executing profile operation"
                        );
                    }
                }
            }

            // If none in this batch made progress, break out to avoid busy-looping
            if !executed_any {
                break;
            }
        }

        Ok(total_handled)
    }
}

#[async_trait]
impl ReconcilePass for ProfileOperationsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
