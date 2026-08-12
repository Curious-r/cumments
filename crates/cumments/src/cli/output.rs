//! Shared stdout helpers for CLI commands.

use anyhow::Result;
use cumments_api::routes::admin::{AdminBlockedRoom, AdminSite};
use serde::Serialize;

/// Prints one JSON document to stdout (machine-readable CLI output).
pub(super) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// Human-readable table for `sites list --table`.
pub(super) fn print_site_table(sites: &[AdminSite]) {
    println!(
        "{:<16} {:<10} {:<12} ORIGINS",
        "SITE_ID", "AUTH_MODE", "STATUS"
    );
    for site in sites {
        let origins = site
            .origins
            .iter()
            .map(|origin| origin.origin.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{:<16} {:<10} {:<12} {}",
            site.site_id,
            site.auth_mode.as_str(),
            site.verification_status.as_str(),
            origins
        );
    }
}

/// Human-readable table for `rooms list-blocked --table`.
pub(super) fn print_room_table(rooms: &[AdminBlockedRoom]) {
    println!(
        "{:<44} {:<16} {:<16} REASON",
        "ROOM_ID", "SITE_ID", "POST_SLUG"
    );
    for room in rooms {
        println!(
            "{:<44} {:<16} {:<16} {}",
            room.room_id, room.site_id, room.post_slug, room.reason
        );
    }
}
