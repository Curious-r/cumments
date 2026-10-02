//! Space membership must not reactivate a retired comment-room instance.
//!
//! Page retirement renames a room and removes its alias, but does not clear
//! the `m.space.child` entry, so an attach event for a retired room can still
//! be observed. Such an event must be ignored, while a live room attached to
//! the same Space still registers normally.

use std::sync::Arc;

use cumments_core::models::{PageSlug, RoomIdentity, RoomStatus, SiteId};
use cumments_core::ports::{RegistryStore, SiteStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::ParsedSpaceChild;
use cumments_store::DbStore;
use tokio::sync::broadcast;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-space-child-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn processor(store: Arc<DbStore>) -> EventProcessor {
    let (tx, _rx) = broadcast::channel(16);
    EventProcessor::new(EventProcessorDeps {
        site_store: store.clone() as Arc<dyn cumments_core::ports::SiteStore>,
        registry_store: store.clone() as Arc<dyn cumments_core::ports::RegistryStore>,
        message_store: store.clone() as Arc<dyn cumments_core::ports::MessageStore>,
        room_store: store.clone() as Arc<dyn cumments_core::ports::RoomStore>,
        governance_store: store.clone() as Arc<dyn cumments_core::ports::GovernanceStore>,
        sticker_pack_store: store.clone() as Arc<dyn cumments_core::ports::StickerPackStore>,
        projection_repair_store: store.clone()
            as Arc<dyn cumments_core::ports::ProjectionRepairStore>,
        role_claim_store: store.clone() as Arc<dyn cumments_core::ports::RoleClaimStore>,
        submission_store: store.clone() as Arc<dyn cumments_core::ports::SubmissionStore>,
        audit_store: store.clone() as Arc<dyn cumments_core::ports::CommandAuditStore>,
        site_auth_store: store.clone() as Arc<dyn cumments_core::ports::SiteAuthStore>,
        site_auth_policy: Arc::new(cumments_core::site_auth::SiteAuthPolicy {
            verification: cumments_core::site_auth::SiteVerificationPolicy::Optional,
            sites: Default::default(),
        }),
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: None,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus: tx,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(tokio::sync::Notify::new()),
        server_name: None,
        historical_state_resolver: Some(Arc::new(
            cumments_test_utils::TestDriver::with_historical_stub(None),
        )),
    })
}

fn attach(child_room_id: &str) -> ParsedSpaceChild {
    ParsedSpaceChild {
        space_room_id: "!space:hs".to_string(),
        site_id: Some("my-blog".to_string()),
        child_room_id: child_room_id.to_string(),
        is_attached: true,
        child_room_identity: Some(RoomIdentity {
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
        }),
    }
}

async fn prepare(name: &str) -> (Arc<DbStore>, SiteId, PageSlug) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("hello");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("provision site");
    (store, site_id, page_slug)
}

#[tokio::test]
async fn space_child_attach_does_not_reactivate_a_retired_room() {
    let (store, site_id, page_slug) = prepare("retired").await;
    store
        .register_room("!retired:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    assert!(
        store
            .mark_room_retired("!retired:hs")
            .await
            .expect("mark retired")
    );

    processor(store.clone())
        .process_space_child(attach("!retired:hs"))
        .await
        .expect("process space child");

    assert_eq!(
        store.get_room_status("!retired:hs").await.expect("status"),
        Some(RoomStatus::Retired),
        "space membership must not reactivate a retired instance"
    );
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        None,
        "a retired room must not become the page's active room"
    );
}

#[tokio::test]
async fn space_child_attach_still_registers_a_live_room() {
    let (store, site_id, page_slug) = prepare("live").await;
    store
        .register_room("!live:hs", &site_id, &page_slug)
        .await
        .expect("register room");

    processor(store.clone())
        .process_space_child(attach("!live:hs"))
        .await
        .expect("process space child");

    assert_eq!(
        store.get_room_status("!live:hs").await.expect("status"),
        Some(RoomStatus::Active),
        "attaching a live room keeps registering it as active"
    );
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        Some("!live:hs".to_string())
    );
}
