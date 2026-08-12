//! `cumments rooms ...` command handling.

use super::args::{RoomsArgs, RoomsCommand};
use super::output::{print_json, print_room_table};
use anyhow::{Result, bail};
use cumments_api::routes::admin::{
    AdminBlockedRoom, AdminListQuery, AdminPage, admin_meta, admin_page_bounds,
};
use cumments_core::ports::RegistryStore;

/// Handles `cumments rooms ...`.
pub async fn handle_rooms_command(store: &cumments_store::DbStore, args: &RoomsArgs) -> Result<()> {
    match &args.command {
        RoomsCommand::ListBlocked(list_args) => {
            let mut rooms = store.get_blocked_rooms().await?;
            rooms.sort_by(|a, b| a.site_id.cmp(&b.site_id).then(a.room_id.cmp(&b.room_id)));
            if let Some(site_id) = list_args.site_id.as_deref().filter(|s| !s.is_empty()) {
                rooms.retain(|room| room.site_id == site_id);
            }
            let query = AdminListQuery {
                page: Some(list_args.page),
                per_page: Some(list_args.per_page),
                site_id: list_args.site_id.clone(),
            };
            let (page, per_page) = admin_page_bounds(&query);
            let total = rooms.len() as i64;
            let start = ((page - 1) * per_page) as usize;
            let data = rooms
                .into_iter()
                .skip(start)
                .take(per_page as usize)
                .map(|room| AdminBlockedRoom {
                    room_id: room.room_id,
                    site_id: room.site_id,
                    post_slug: room.post_slug,
                    reason: room.reason,
                    updated_at: room.updated_at,
                })
                .collect::<Vec<_>>();
            let page = AdminPage {
                data,
                meta: admin_meta(total, page, per_page),
            };
            if list_args.table {
                print_room_table(&page.data);
            } else {
                print_json(&page)?;
            }
            Ok(())
        }
        RoomsCommand::Unblock(args) => {
            let unblocked = store.unblock_room(&args.room_id).await?;
            if !unblocked {
                bail!("room not found in the registry");
            }
            print_json(&serde_json::json!({
                "room_id": args.room_id,
                "unblocked": true,
            }))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::{BlockedListArgs, UnblockRoomArgs};
    use super::super::test_support::*;
    use super::*;
    use cumments_core::models::{PostSlug, SiteId};
    use cumments_store::DbStore;

    #[tokio::test]
    async fn rooms_list_blocked_and_unblock() {
        let store = DbStore::connect(&test_db_url("rooms"))
            .await
            .expect("connect db");
        let site = SiteId::from("my-blog");
        let slug = PostSlug::from("hello");
        store
            .register_room("!room:hs", &site, &slug)
            .await
            .expect("register room");
        store
            .mark_room_blocked("!room:hs", "Refusing to adopt room")
            .await
            .expect("mark blocked");

        let list = RoomsArgs {
            command: RoomsCommand::ListBlocked(BlockedListArgs {
                site_id: None,
                page: 1,
                per_page: 20,
                table: false,
            }),
        };
        handle_rooms_command(&store, &list)
            .await
            .expect("list blocked rooms");

        let unblock = RoomsArgs {
            command: RoomsCommand::Unblock(UnblockRoomArgs {
                room_id: "!room:hs".to_string(),
            }),
        };
        handle_rooms_command(&store, &unblock)
            .await
            .expect("unblock room");
        assert!(
            store
                .get_blocked_rooms()
                .await
                .expect("blocked rooms")
                .is_empty(),
            "room must no longer be blocked"
        );

        let missing = RoomsArgs {
            command: RoomsCommand::Unblock(UnblockRoomArgs {
                room_id: "!nope:hs".to_string(),
            }),
        };
        assert!(
            handle_rooms_command(&store, &missing).await.is_err(),
            "unknown room must fail"
        );
    }
}
