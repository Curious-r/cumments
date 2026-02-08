use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::PostCommentIntent;
use tracing::info;

use crate::MatrixOperator;

/// A dummy operator for testing and local development.
/// It doesn't actually connect to Matrix, but instead logs the actions
/// it would have taken.
pub struct LoggingOperator;

#[async_trait]
impl MatrixOperator for LoggingOperator {
    async fn post_comment(&self, intent: &PostCommentIntent) -> Result<String> {
        info!(
            "[LoggingOperator] Would post comment to site '{}', post '{}': '{}'",
            intent.site_id.as_str(),
            intent.post_slug.as_str(),
            intent.content
        );

        // Return a fake event ID
        Ok("$fake_event_id_for_logging_operator".to_string())
    }
}
