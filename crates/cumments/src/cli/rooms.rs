//! `cumments rooms ...` command handling.

use super::args::{RetireRoomArgs, RoomsArgs, RoomsCommand, UpgradeRoomArgs};
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
        RoomsCommand::Upgrade(_) => unreachable!("upgrade is handled after driver setup"),
        RoomsCommand::Retire(args) => retire_room(store, args).await,
    }
}

/// Marks a room `Retired` (writes stop immediately). The running server's
/// reconciler performs the Matrix retirement and local cleanup.
async fn retire_room(store: &cumments_store::DbStore, args: &RetireRoomArgs) -> Result<()> {
    if !args.yes {
        bail!("refusing to retire the room without `--yes`");
    }
    let retired =
        cumments_core::management::retire_post_room_by_room_id(store, &args.room_id).await?;
    if !retired {
        bail!("room not found or not active");
    }

    if args.wait {
        for _ in 0..300 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if store
                .get_registered_room_identity(&args.room_id)
                .await?
                .is_none()
            {
                print_json(&serde_json::json!({
                    "room_id": args.room_id,
                    "status": "retired",
                }))?;
                return Ok(());
            }
        }
        bail!("timed out waiting for the retirement to finish");
    }

    print_json(&serde_json::json!({
        "room_id": args.room_id,
        "status": "retiring",
    }))?;
    eprintln!(
        "The running server retires the Matrix room in the background; \
         re-run with `--wait` to block until it finishes."
    );
    Ok(())
}

/// Handles `cumments rooms upgrade ...` after the Matrix driver exists.
pub async fn handle_rooms_upgrade_command(
    store: &cumments_store::DbStore,
    driver: &dyn cumments_core::ports::MatrixDriver,
    site_service: &cumments_core::site_service::SiteService,
    args: &UpgradeRoomArgs,
) -> Result<()> {
    let replacement = cumments_core::management::upgrade_comment_room(
        driver,
        store,
        site_service,
        &args.room_id,
        &args.new_version,
    )
    .await?;
    print_json(&serde_json::json!({
        "room_id": args.room_id,
        "new_version": args.new_version,
        "replacement_room": replacement,
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::args::{QuarantinedListArgs, ReinstateRoomArgs, RetireRoomArgs};
    use super::super::test_support::*;
    use super::*;
    use cumments_core::models::{PostSlug, RoomStatus, SiteId};
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

    #[tokio::test]
    async fn rooms_retire_requires_yes_and_marks_retired() {
        let store = DbStore::connect(&test_db_url("rooms-retire"))
            .await
            .expect("connect db");
        let site = SiteId::from("my-blog");
        let slug = PostSlug::from("hello");
        store
            .register_room("!room:hs", &site, &slug)
            .await
            .expect("register room");

        let no_confirm = RoomsArgs {
            command: RoomsCommand::Retire(RetireRoomArgs {
                room_id: "!room:hs".to_string(),
                yes: false,
                wait: false,
            }),
        };
        assert!(
            handle_rooms_command(&store, &no_confirm).await.is_err(),
            "retire without --yes must fail"
        );
        assert_eq!(
            store
                .get_room_status("!room:hs")
                .await
                .expect("room status"),
            Some(RoomStatus::Active)
        );

        let retire = RoomsArgs {
            command: RoomsCommand::Retire(RetireRoomArgs {
                room_id: "!room:hs".to_string(),
                yes: true,
                wait: false,
            }),
        };
        handle_rooms_command(&store, &retire)
            .await
            .expect("retire room");
        assert_eq!(
            store
                .get_room_status("!room:hs")
                .await
                .expect("room status"),
            Some(RoomStatus::Retired)
        );
    }
}
