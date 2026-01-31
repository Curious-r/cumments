use matrix_sdk::{Client, Room};
use storage::Db;

pub async fn ensure_profile_cached(
    db: &Db,
    client: &Client,
    room: Option<&Room>,
    user_id: &str,
) -> (String, Option<String>) {
    let mut name = user_id.to_string();
    let mut avatar = None;

    if let Ok(Some(cached)) = db.get_cached_profile(user_id).await {
        if let Some(n) = cached.display_name {
            name = n;
        }
        return (name, cached.avatar_url);
    }

    let mut fetched = false;
    let uid = match matrix_sdk::ruma::UserId::parse(user_id) {
        Ok(u) => u,
        Err(_) => return (name, avatar),
    };

    if let Some(r) = room {
        if let Ok(Some(member)) = r.get_member_no_sync(&uid).await {
            if let Some(n) = member.display_name() {
                name = n.to_string();
            }
            avatar = member.avatar_url().map(|s| s.to_string());
            fetched = true;
        }
    }

    if !fetched {
        if let Ok(resp) = client.get_profile(&uid).await {
            if let Some(n) = resp.displayname {
                name = n;
            }
            avatar = resp.avatar_url.map(|s| s.to_string());
        }
    }

    let _ = db
        .upsert_profile(user_id, Some(&name), avatar.as_deref())
        .await;

    (name, avatar)
}
