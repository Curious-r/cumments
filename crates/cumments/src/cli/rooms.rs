//! `cumments rooms ...` command handling.

use super::args::{RoomsArgs, RoomsCommand};
use super::output::{print_json, print_room_table};
use anyhow::{Result, bail};
use cumments_core::operator::{OperatorListQuery, list_operator_quarantined_rooms};
use cumments_core::ports::RegistryStore;

/// Handles `cumments rooms ...`.
pub async fn handle_rooms_command(store: &cumments_store::DbStore, args: &RoomsArgs) -> Result<()> {
    match &args.command {
        RoomsCommand::ListQuarantined(list_args) => {
            let query = OperatorListQuery {
                page: Some(list_args.page),
                per_page: Some(list_args.per_page),
                site_id: list_args.site_id.clone(),
            };
            let page = list_operator_quarantined_rooms(store, &query).await?;
            if list_args.table {
                print_room_table(&page.data);
            } else {
                print_json(&page)?;
            }
            Ok(())
        }
        RoomsCommand::Reinstate(args) => {
            let reinstated = store.reinstate_room(&args.room_id).await?;
            if !reinstated {
                bail!("room not found in the registry");
            }
            print_json(&serde_json::json!({
                "room_id": args.room_id,
                "status": "active",
            }))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::{QuarantinedListArgs, ReinstateRoomArgs};
    use super::super::test_support::*;
    use super::*;
    use cumments_core::models::{PostSlug, SiteId};
    use cumments_store::DbStore;

    #[tokio::test]
    async fn rooms_list_quarantined_and_reinstate() {
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
            .quarantine_room("!room:hs", "Refusing to adopt room", 1, None)
            .await
            .expect("quarantine room");

        let list = RoomsArgs {
            command: RoomsCommand::ListQuarantined(QuarantinedListArgs {
                site_id: None,
                page: 1,
                per_page: 20,
                table: false,
            }),
        };
        handle_rooms_command(&store, &list)
            .await
            .expect("list quarantined rooms");

        let reinstate = RoomsArgs {
            command: RoomsCommand::Reinstate(ReinstateRoomArgs {
                room_id: "!room:hs".to_string(),
            }),
        };
        handle_rooms_command(&store, &reinstate)
            .await
            .expect("reinstate room");
        assert!(
            store
                .get_quarantined_rooms()
                .await
                .expect("quarantined rooms")
                .is_empty(),
            "room must no longer be quarantined"
        );

        let missing = RoomsArgs {
            command: RoomsCommand::Reinstate(ReinstateRoomArgs {
                room_id: "!nope:hs".to_string(),
            }),
        };
        assert!(
            handle_rooms_command(&store, &missing).await.is_err(),
            "unknown room must fail"
        );
    }
}
