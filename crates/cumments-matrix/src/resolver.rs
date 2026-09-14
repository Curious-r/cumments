//! Matrix implementation of [`HistoricalRoomStateResolver`].

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::MemberPresentation;
use cumments_core::ports::HistoricalRoomStateResolver;

use crate::appservice::AppServiceMatrixDriver;

/// Concrete driver implementing [`HistoricalRoomStateResolver`] using Matrix homeserver state facilities.
pub struct MatrixHistoricalStateResolver {
    driver: Arc<AppServiceMatrixDriver>,
}

impl MatrixHistoricalStateResolver {
    pub fn new(driver: Arc<AppServiceMatrixDriver>) -> Self {
        Self { driver }
    }
}

#[async_trait]
impl HistoricalRoomStateResolver for MatrixHistoricalStateResolver {
    async fn resolve_member_presentation(
        &self,
        room_id: &str,
        event_id: &str,
        sender_mxid: &str,
    ) -> Result<Option<MemberPresentation>> {
        self.driver
            .resolve_member_presentation(room_id, event_id, sender_mxid)
            .await
    }
}
