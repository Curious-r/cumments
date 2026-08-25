//! Repairs Matrix facts whose local projection failed closed.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tracing::{info, warn};

/// Upper bound of repair work per sweep.
const REPAIR_BATCH_SIZE: u64 = 100;

/// Schedules the next authoritative fetch after a failed attempt.
fn retry_after_attempt(attempt: u32) -> chrono::DateTime<chrono::Utc> {
    let delays = [
        Duration::from_secs(5),
        Duration::from_secs(30),
        Duration::from_secs(2 * 60),
        Duration::from_secs(10 * 60),
    ];
    let delay = delays
        .get(attempt.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(*delays.last().expect("non-empty retry schedule"));
    chrono::Utc::now() + chrono::Duration::from_std(delay).expect("valid retry delay")
}

pub struct ProjectionRepairsPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl ProjectionRepairsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }
}

#[async_trait]
impl ReconcilePass for ProjectionRepairsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        let mut handled = 0u64;
        loop {
            let repairs = self
                .deps
                .projection_repair_store
                .claim_due_projection_repairs(REPAIR_BATCH_SIZE)
                .await?;
            if repairs.is_empty() {
                break;
            }

            for repair in repairs {
                handled += 1;
                match self
                    .deps
                    .state_redaction_repairer
                    .repair_state_redaction(&repair.target_event_id)
                    .await
                {
                    Ok(()) => {}
                    Err(error) => {
                        warn!(
                            target_event_id = %repair.target_event_id,
                            attempt = repair.attempts,
                            error = %error,
                            "Projection repair failed"
                        );
                        self.deps
                            .projection_repair_store
                            .record_projection_repair_failure(
                                &repair.target_event_id,
                                &error.to_string(),
                                retry_after_attempt(repair.attempts),
                            )
                            .await?;
                    }
                }
            }
        }

        if handled > 0 {
            info!(handled, "Handled projection repairs");
        }
        Ok(handled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_are_bounded_and_monotonic() {
        let now = chrono::Utc::now();
        let mut previous = now;
        for attempt in 1..=6u32 {
            let next = retry_after_attempt(attempt);
            assert!(next > previous);
            previous = next;
        }
        assert!(
            previous - now <= chrono::Duration::seconds(11 * 60),
            "the final delay must remain bounded"
        );
    }
}
