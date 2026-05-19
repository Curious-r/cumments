use crate::models::{Site, SiteId};
use crate::ports::{MatrixDriver, SiteStore};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// A Domain Service that manages the lifecycle of Matrix Spaces for sites.
/// This is the "Brain" for site-related logic.
pub struct SiteService {
    store: Arc<dyn SiteStore>,
    cache: Arc<RwLock<HashMap<String, String>>>,
}

impl SiteService {
    /// Creates a new SiteService.
    pub fn new(store: Arc<dyn SiteStore>) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Ensures a Matrix Space exists for the given site and returns its room ID.
    /// This method coordinates between the database, cache, and the Matrix driver.
    pub async fn ensure_space(
        &self,
        site_id: &SiteId,
        driver: &dyn MatrixDriver,
    ) -> Result<String> {
        let site_id_str = site_id.as_str();

        // 1. Check memory cache
        {
            let cache = self.cache.read().await;
            if let Some(space_id) = cache.get(site_id_str) {
                return Ok(space_id.clone());
            }
        }

        // 2. Check Store
        if let Some(site) = self.store.get_site(site_id).await? {
            let space_id = site.matrix_space_id;
            // Update cache
            self.cache
                .write()
                .await
                .insert(site_id_str.to_string(), space_id.clone());
            return Ok(space_id);
        }

        // 3. Command Driver to create a new Space in Matrix
        info!(
            "Site {} not found in store, commanding Driver to create a new Space",
            site_id_str
        );
        let space_id = driver.create_site_space(site_id).await?;

        // 4. Persist the new mapping to Store
        let new_site = Site {
            id: site_id_str.to_string(),
            matrix_space_id: space_id.clone(),
            display_name: Some(site_id_str.to_string()),
            created_at: chrono::Utc::now(),
        };
        self.store.save_site(&new_site).await?;

        // 5. Update memory cache
        self.cache
            .write()
            .await
            .insert(site_id_str.to_string(), space_id.clone());

        Ok(space_id)
    }
}
