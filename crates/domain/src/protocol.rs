use crate::models::SiteId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug)]
pub struct CummentsMetadata {
    pub author_name: String,
    pub is_guest: bool,
    pub origin_content: String,
    pub author_fingerprint: Option<String>,
}

pub fn parse_room_alias(localpart: &str) -> Option<(SiteId, String)> {
    let localpart = localpart.trim_start_matches('#');
    let (site_id_str, slug) = localpart.split_once('_')?;
    let site_id = SiteId::new(site_id_str).ok()?;
    Some((site_id, slug.to_string()))
}

pub fn build_outbound_event(nickname: &str, content: &str, fingerprint: Option<String>) -> Value {
    let body_fallback = format!("**{}** (Guest): {}", nickname, content);
    let metadata = CummentsMetadata {
        author_name: nickname.to_string(),
        is_guest: true,
        origin_content: content.to_string(),
        author_fingerprint: fingerprint,
    };

    serde_json::json!({
        "msgtype": "m.text",
        "body": body_fallback,
        "com.cumments.v1": metadata
    })
}

pub fn extract_comment_data(
    content_json: &Value,
    sender_id: &str,
    bot_id: &str,
) -> (String, bool, String, Option<String>) {
    if let Some(metadata_val) = content_json.get("com.cumments.v1") {
        if let Ok(meta) = serde_json::from_value::<CummentsMetadata>(metadata_val.clone()) {
            return (
                meta.author_name,
                meta.is_guest,
                meta.origin_content,
                meta.author_fingerprint,
            );
        }
    }

    let body = content_json
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if sender_id == bot_id {
        let parts: Vec<&str> = body.splitn(2, " (Guest): ").collect();
        if parts.len() == 2 {
            let nick = parts[0]
                .trim_start_matches("**")
                .trim_end_matches("**")
                .to_string();
            return (nick, true, parts[1].to_string(), None);
        }
        return ("Bot".to_string(), false, body.to_string(), None);
    }

    (sender_id.to_string(), false, body.to_string(), None)
}
