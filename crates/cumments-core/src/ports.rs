//! Defines the "Ports" of our application, following the Hexagonal Architecture pattern.
//! These are traits that define the contracts for interacting with the outside world
//! (e.g., storage, the Matrix network). The core business logic will depend on these
//! traits, and the infrastructure crates will provide the concrete implementations.

use crate::intents::PostCommentIntent;
use anyhow::Result;
use async_trait::async_trait;

/// The port for all intent storage operations.
/// This contract is implemented by the `cumments-storage` crate.
#[async_trait]
pub trait IntentRepository {
    /// Saves a `PostCommentIntent` to the persistent queue for the
    /// reconciler to process.
    async fn save_post_comment_intent(&self, intent: &PostCommentIntent) -> Result<()>;
}
