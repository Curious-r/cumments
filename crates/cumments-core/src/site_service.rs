use crate::models::{Site, SiteId};
use crate::ports::{MatrixDriver, SiteStore};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::info;

/// Upper bound for the in-memory site caches. The caches are pure
/// accelerators; the store remains the source of truth, so resetting them is
/// always safe.
const MAX_CACHE_ENTRIES: usize = 4096;

/// A shared service that manages the lifecycle of Matrix Spaces for sites.
pub struct SiteService {
    store: Arc<dyn SiteStore>,
    cache: Arc<RwLock<HashMap<String, String>>>,
    /// Per-site locks so concurrent first-use of the same site cannot race two
    /// createRoom calls; the loser re-checks cache/store and adopts the winner.
    inflight: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl SiteService {
    /// Creates a new SiteService.
    pub fn new(store: Arc<dyn SiteStore>) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashMap::new())),
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

        // Serialize concurrent first-use per site. Entries are only dropped
        // once idle and the map exceeds its cap; active or waiting locks keep
        // their Arc alive so the guard stays valid for waiters.
        let site_lock = {
            let mut inflight = self.inflight.lock().await;
            if inflight.len() > MAX_CACHE_ENTRIES {
                inflight.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            inflight
                .entry(site_id_str.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = site_lock.lock().await;

        // 1. Check memory cache
        {
            let cache = self.cache.read().await;
            if let Some(space_id) = cache.get(site_id_str) {
                return Ok(space_id.clone());
            }
        }

        // 2. Check Store. A blank space ID means the site was pre-registered
        // through the API but never provisioned in Matrix yet; treat it as
        // missing so the driver creates the Space.
        if let Some(site) = self.store.get_site(site_id).await?
            && !site.matrix_space_id.is_empty()
        {
            let space_id = site.matrix_space_id;
            // Update cache
            self.cache_put(site_id_str.to_string(), space_id.clone())
                .await;
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
        self.cache_put(site_id_str.to_string(), space_id.clone())
            .await;

        Ok(space_id)
    }

    /// Returns the site's Matrix Space room ID without provisioning one.
    /// `None` means the site has no Space yet.
    pub async fn space_id(&self, site_id: &SiteId) -> Result<Option<String>> {
        let site_id_str = site_id.as_str();
        if let Some(space_id) = self.cache.read().await.get(site_id_str).cloned() {
            return Ok(Some(space_id));
        }
        if let Some(site) = self.store.get_site(site_id).await?
            && !site.matrix_space_id.is_empty()
        {
            let space_id = site.matrix_space_id;
            self.cache_put(site_id_str.to_string(), space_id.clone())
                .await;
            return Ok(Some(space_id));
        }
        Ok(None)
    }

    async fn cache_put(&self, key: String, value: String) {
        let mut cache = self.cache.write().await;
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, value);
    }
}
