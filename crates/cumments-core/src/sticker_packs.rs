//! Site sticker packs (MSC2545) domain model and validation.
//!
//! The authoritative data lives in `m.room.image_pack` state events on a
//! site's Space. This module only contains pure helpers that shape and
//! validate that content; persistence and Matrix access live elsewhere.

use crate::models::SiteId;
use crate::ports::{MatrixDriver, SiteStore, StickerPackStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

/// The Matrix state event type carrying an image pack (MSC2545).
pub const IMAGE_PACK_EVENT_TYPE: &str = "m.room.image_pack";

/// The pack id Cumments uses for a site's default pack.
pub const DEFAULT_PACK_ID: &str = "default";

/// Shortcode length limit from MSC2545.
pub const MAX_SHORTCODE_BYTES: usize = 100;

/// Practical cap for one mxc URL referenced by a pack image.
pub const MAX_MXC_URL_LEN: usize = 512;

/// The Matrix event size limit; packs are full-state events and must fit.
pub const MAX_PACK_EVENT_BYTES: usize = 64 * 1024;

/// One image inside a sticker pack (the normalized `images` map value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerImage {
    pub shortcode: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
}

/// Normalized `m.room.image_pack` content after validation. This is what the
/// SQLite projection stores (as `pack_json`); Matrix remains the authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StickerPackContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub usage: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(default)]
    pub images: Vec<StickerImage>,
}

/// A sticker pack with its Matrix context (which Space, which state key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerPack {
    pub room_id: String,
    pub site_id: String,
    pub state_key: String,
    pub content: StickerPackContent,
}

/// A projected pack plus the Matrix event it was derived from. Used by the
/// projector to upsert and by the API to serve the read model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerPackProjection {
    pub pack: StickerPack,
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StickerPackError {
    #[error("pack content must be a JSON object")]
    NotAnObject,
    #[error("pack content exceeds the Matrix event size limit")]
    PackTooLarge,
    #[error("invalid shortcode `{0}`")]
    InvalidShortcode(String),
    #[error("invalid mxc URL `{0}`")]
    InvalidMxc(String),
    #[error("invalid pack metadata: {0}")]
    InvalidMetadata(String),
}

/// Errors from the site sticker-pack use cases shared by API and bot.
#[derive(Debug, Error)]
pub enum StickerPackUseCaseError {
    #[error("invalid sticker pack input: {0}")]
    Invalid(#[from] StickerPackError),
    #[error("site {0} not found")]
    SiteNotFound(String),
    #[error("site {0} has no Matrix Space")]
    SiteWithoutSpace(String),
    #[error("sticker pack `{0}` not found")]
    PackNotFound(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Whether a shortcode satisfies the MSC2545 grammar (`[a-zA-Z0-9-_]+`,
/// at most 100 bytes).
pub fn validate_shortcode(shortcode: &str) -> Result<(), StickerPackError> {
    if shortcode.is_empty()
        || shortcode.len() > MAX_SHORTCODE_BYTES
        || !shortcode
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StickerPackError::InvalidShortcode(shortcode.to_string()));
    }
    Ok(())
}

/// Splits an `mxc://server/media_id` URL into its parts.
pub fn parse_mxc_url(url: &str) -> Result<(&str, &str), StickerPackError> {
    if url.len() > MAX_MXC_URL_LEN || url.contains(|c: char| c.is_control()) {
        return Err(StickerPackError::InvalidMxc(url.to_string()));
    }
    let rest = url
        .strip_prefix("mxc://")
        .ok_or_else(|| StickerPackError::InvalidMxc(url.to_string()))?;
    let Some((server, media_id)) = rest.split_once('/') else {
        return Err(StickerPackError::InvalidMxc(url.to_string()));
    };
    if server.is_empty() || media_id.is_empty() {
        return Err(StickerPackError::InvalidMxc(url.to_string()));
    }
    Ok((server, media_id))
}

/// Whether a pack's `usage` includes stickers. Absent/empty usage means all
/// usages per MSC2545.
pub fn is_sticker_usage(usage: &[String]) -> bool {
    usage.is_empty() || usage.iter().any(|u| u == "sticker")
}

/// Validates a pack id for the API/bot write paths (a path segment and a
/// Matrix `state_key` that stays URL-friendly).
pub fn validate_pack_id(pack_id: &str) -> Result<(), StickerPackError> {
    if pack_id.is_empty()
        || pack_id.len() > 100
        || !pack_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(StickerPackError::InvalidMetadata(format!(
            "invalid pack id `{pack_id}`"
        )));
    }
    Ok(())
}

/// Serializes normalized pack content back into the `m.room.image_pack`
/// content object Cumments writes to Matrix.
pub fn sticker_pack_content_to_value(content: &StickerPackContent) -> Value {
    let images = content
        .images
        .iter()
        .map(|image| {
            let mut object = serde_json::Map::new();
            object.insert("url".to_string(), Value::String(image.url.clone()));
            if let Some(body) = &image.body {
                object.insert("body".to_string(), Value::String(body.clone()));
            }
            if let Some(info) = &image.info {
                object.insert("info".to_string(), info.clone());
            }
            (image.shortcode.clone(), Value::Object(object))
        })
        .collect::<serde_json::Map<_, _>>();

    let mut pack = serde_json::Map::new();
    if let Some(name) = &content.display_name {
        pack.insert("display_name".to_string(), Value::String(name.clone()));
    }
    if let Some(avatar) = &content.avatar_url {
        pack.insert("avatar_url".to_string(), Value::String(avatar.clone()));
    }
    if !content.usage.is_empty() {
        pack.insert(
            "usage".to_string(),
            Value::Array(
                content
                    .usage
                    .iter()
                    .map(|u| Value::String(u.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(attribution) = &content.attribution {
        pack.insert(
            "attribution".to_string(),
            Value::String(attribution.clone()),
        );
    }

    let mut root = serde_json::Map::new();
    root.insert("images".to_string(), Value::Object(images));
    root.insert("pack".to_string(), Value::Object(pack));
    Value::Object(root)
}

/// Lists the projected sticker packs of one site.
pub async fn list_site_sticker_packs(
    store: &dyn StickerPackStore,
    site_id: &str,
) -> Result<Vec<StickerPackProjection>, StickerPackUseCaseError> {
    Ok(store.list_site_packs(site_id).await?)
}

/// Input for [`add_site_sticker`].
pub struct AddStickerInput<'a> {
    pub site_id: &'a str,
    pub pack_id: &'a str,
    pub shortcode: &'a str,
    pub url: &'a str,
    pub body: Option<String>,
    pub info: Option<Value>,
}

/// Adds or replaces one sticker image in a site's pack (read-modify-write of
/// the full `m.room.image_pack` state event). Creating a new pack is
/// implicit; the pack is marked as a sticker pack.
pub async fn add_site_sticker(
    sites: &dyn SiteStore,
    driver: &dyn MatrixDriver,
    input: AddStickerInput<'_>,
) -> Result<StickerPackProjection, StickerPackUseCaseError> {
    let AddStickerInput {
        site_id,
        pack_id,
        shortcode,
        url,
        body,
        info,
    } = input;
    SiteId::new(site_id.to_string())
        .map_err(|_| StickerPackError::InvalidMetadata("invalid site id".into()))?;
    validate_pack_id(pack_id)?;
    validate_shortcode(shortcode)?;
    parse_mxc_url(url)?;
    if let Some(ref body) = body
        && (body.is_empty() || body.len() > 2048)
    {
        return Err(StickerPackError::InvalidMetadata("body is empty or too long".into()).into());
    }
    if let Some(ref info) = info
        && !info.is_object()
    {
        return Err(StickerPackError::InvalidMetadata("info must be an object".into()).into());
    }

    let (space_id, mut content) =
        resolve_pack_content(sites, driver, site_id, pack_id, true).await?;
    if content.usage.is_empty() {
        content.usage = vec!["sticker".to_string()];
    }
    if content.display_name.is_none() && content.images.is_empty() {
        content.display_name = Some(pack_id.to_string());
    }
    if let Some(existing) = content.images.iter_mut().find(|i| i.shortcode == shortcode) {
        existing.url = url.to_string();
        existing.body = body.clone();
        existing.info = info.clone();
    } else {
        content.images.push(StickerImage {
            shortcode: shortcode.to_string(),
            url: url.to_string(),
            body,
            info,
        });
        content.images.sort_by(|a, b| a.shortcode.cmp(&b.shortcode));
    }

    write_pack(driver, site_id, &space_id, pack_id, content).await
}

/// Removes one sticker image from a site's pack. Idempotent: removing a
/// shortcode that is not present still rewrites the current state unchanged.
pub async fn remove_site_sticker(
    sites: &dyn SiteStore,
    driver: &dyn MatrixDriver,
    site_id: &str,
    pack_id: &str,
    shortcode: &str,
) -> Result<StickerPackProjection, StickerPackUseCaseError> {
    SiteId::new(site_id.to_string())
        .map_err(|_| StickerPackError::InvalidMetadata("invalid site id".into()))?;
    validate_pack_id(pack_id)?;
    validate_shortcode(shortcode)?;

    let (space_id, mut content) =
        resolve_pack_content(sites, driver, site_id, pack_id, false).await?;
    content.images.retain(|image| image.shortcode != shortcode);
    write_pack(driver, site_id, &space_id, pack_id, content).await
}

/// Reads the current pack content (or an empty default for a new pack) for a
/// site's Space.
async fn resolve_pack_content(
    sites: &dyn SiteStore,
    driver: &dyn MatrixDriver,
    site_id: &str,
    pack_id: &str,
    allow_create: bool,
) -> Result<(String, StickerPackContent), StickerPackUseCaseError> {
    let site = sites
        .get_site(
            &SiteId::new(site_id.to_string())
                .map_err(|_| StickerPackError::InvalidMetadata("invalid site id".into()))?,
        )
        .await?
        .ok_or_else(|| StickerPackUseCaseError::SiteNotFound(site_id.to_string()))?;
    if site.matrix_space_id.is_empty() {
        return Err(StickerPackUseCaseError::SiteWithoutSpace(
            site_id.to_string(),
        ));
    }
    let space_id = site.matrix_space_id;
    let current = driver
        .get_room_state(&space_id, IMAGE_PACK_EVENT_TYPE, pack_id)
        .await?;
    let content = match current.as_ref() {
        None if allow_create => StickerPackContent::default(),
        None => {
            return Err(StickerPackUseCaseError::PackNotFound(pack_id.to_string()));
        }
        Some(current) => match parse_image_pack_content(&space_id, site_id, pack_id, current) {
            Ok(Some(pack)) => pack.content,
            Ok(None) => {
                return Err(StickerPackError::InvalidMetadata(format!(
                    "pack `{pack_id}` does not target stickers"
                ))
                .into());
            }
            Err(error) if allow_create => {
                warn!(
                    "Replacing malformed sticker pack {site_id}/{pack_id} during a write: {error}"
                );
                StickerPackContent::default()
            }
            Err(error) => {
                return Err(StickerPackError::InvalidMetadata(format!(
                    "existing pack `{pack_id}` is malformed: {error}"
                ))
                .into());
            }
        },
    };
    Ok((space_id, content))
}

/// Validates and writes the full pack state, returning the projection.
async fn write_pack(
    driver: &dyn MatrixDriver,
    site_id: &str,
    space_id: &str,
    pack_id: &str,
    content: StickerPackContent,
) -> Result<StickerPackProjection, StickerPackUseCaseError> {
    let value = sticker_pack_content_to_value(&content);
    if value.to_string().len() > MAX_PACK_EVENT_BYTES {
        return Err(StickerPackError::PackTooLarge.into());
    }
    let event_id = driver
        .set_room_state(space_id, IMAGE_PACK_EVENT_TYPE, pack_id, &value)
        .await?;
    Ok(StickerPackProjection {
        pack: StickerPack {
            room_id: space_id.to_string(),
            site_id: site_id.to_string(),
            state_key: pack_id.to_string(),
            content,
        },
        event_id,
        sender: driver.sender_user_id().unwrap_or_default(),
        origin_server_ts: chrono::Utc::now().timestamp_millis(),
    })
}

/// Parses and validates an `m.room.image_pack` content object.
///
/// Returns `Ok(None)` when the pack is well-formed but its `usage` explicitly
/// excludes stickers, so the projector can skip it without treating it as
/// malformed. Invalid individual images are dropped with a warning; the
/// remaining valid images form the projected pack.
pub fn parse_image_pack_content(
    room_id: &str,
    site_id: &str,
    state_key: &str,
    content: &Value,
) -> Result<Option<StickerPack>, StickerPackError> {
    let object = content.as_object().ok_or(StickerPackError::NotAnObject)?;
    if content.to_string().len() > MAX_PACK_EVENT_BYTES {
        return Err(StickerPackError::PackTooLarge);
    }

    let mut metadata = StickerPackContent::default();
    if let Some(pack) = object.get("pack").and_then(Value::as_object) {
        if let Some(name) = pack.get("display_name").and_then(Value::as_str) {
            metadata.display_name = Some(name.to_string());
        }
        if let Some(avatar) = pack.get("avatar_url").and_then(Value::as_str) {
            parse_mxc_url(avatar).map_err(|_| {
                StickerPackError::InvalidMetadata("avatar_url is not a valid mxc URL".into())
            })?;
            metadata.avatar_url = Some(avatar.to_string());
        }
        if let Some(usage) = pack.get("usage").and_then(Value::as_array) {
            metadata.usage = usage
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
        }
        if let Some(attribution) = pack.get("attribution").and_then(Value::as_str) {
            metadata.attribution = Some(attribution.to_string());
        }
    }

    if !is_sticker_usage(&metadata.usage) {
        return Ok(None);
    }

    let mut images = Vec::new();
    if let Some(images_map) = object.get("images").and_then(Value::as_object) {
        let mut entries: Vec<(&String, &Value)> = images_map.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (shortcode, image) in entries {
            let Ok(()) = validate_shortcode(shortcode) else {
                warn!(
                    "Dropping sticker pack image with invalid shortcode in {room_id}/{state_key}: {shortcode:?}"
                );
                continue;
            };
            let Some(url) = image.get("url").and_then(Value::as_str) else {
                warn!(
                    "Dropping sticker pack image {shortcode:?} in {room_id}/{state_key}: missing or non-string url"
                );
                continue;
            };
            if let Err(error) = parse_mxc_url(url) {
                warn!(
                    "Dropping sticker pack image {shortcode:?} in {room_id}/{state_key}: {error}"
                );
                continue;
            }
            let body = image
                .get("body")
                .and_then(Value::as_str)
                .map(str::to_string);
            let info = image.get("info").filter(|v| v.is_object()).cloned();
            images.push(StickerImage {
                shortcode: shortcode.clone(),
                url: url.to_string(),
                body,
                info,
            });
        }
    }
    metadata.images = images;

    Ok(Some(StickerPack {
        room_id: room_id.to_string(),
        site_id: site_id.to_string(),
        state_key: state_key.to_string(),
        content: metadata,
    }))
}

/// Shapes one projected pack for the public sticker API.
///
/// `proxify` maps an `mxc://` URL to a signed preview URL (or `None` when no
/// proxy is configured). `proxify_avatar` maps the pack avatar to its
/// thumbnail variant; callers own both policies.
pub fn pack_response_shape(
    pack: &StickerPack,
    proxify: impl Fn(&str) -> Option<String>,
    proxify_avatar: impl Fn(&str) -> Option<String>,
) -> Value {
    let images = pack
        .content
        .images
        .iter()
        .map(|image| {
            let mut entry = serde_json::json!({
                "shortcode": image.shortcode,
                "url": image.url,
                "proxy_url": proxify(&image.url).unwrap_or_else(|| image.url.clone()),
            });
            if let Some(body) = &image.body {
                entry["body"] = Value::String(body.clone());
            }
            if let Some(info) = &image.info {
                entry["info"] = info.clone();
            }
            entry
        })
        .collect::<Vec<_>>();

    let mut response = serde_json::json!({
        "pack_id": pack.state_key,
        "images": images,
    });
    if let Some(name) = &pack.content.display_name {
        response["display_name"] = Value::String(name.clone());
    }
    if let Some(avatar) = &pack.content.avatar_url {
        response["avatar_url"] = Value::String(avatar.clone());
        response["avatar_proxy_url"] =
            Value::String(proxify_avatar(avatar).unwrap_or_else(|| avatar.clone()));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pack(
        site: &str,
        state_key: &str,
        content: Value,
    ) -> Result<Option<StickerPack>, StickerPackError> {
        parse_image_pack_content("!space:hs", site, state_key, &content)
    }

    #[test]
    fn shortcode_grammar_and_length() {
        assert!(validate_shortcode("cat").is_ok());
        assert!(validate_shortcode("cat_wave-2").is_ok());
        assert!(validate_shortcode("").is_err());
        assert!(validate_shortcode(":cat:").is_err());
        assert!(validate_shortcode("猫").is_err());
        assert!(validate_shortcode(&"a".repeat(101)).is_err());
        assert!(validate_shortcode(&"a".repeat(100)).is_ok());
    }

    #[test]
    fn mxc_url_parses_and_rejects_malformed() {
        assert_eq!(
            parse_mxc_url("mxc://example.org/abc"),
            Ok(("example.org", "abc"))
        );
        assert!(parse_mxc_url("https://example.org/abc").is_err());
        assert!(parse_mxc_url("mxc://example.org/").is_err());
        assert!(parse_mxc_url("mxc:///abc").is_err());
        assert!(parse_mxc_url(&format!("mxc://h/{}", "a".repeat(600))).is_err());
    }

    #[test]
    fn parses_full_pack_and_drops_invalid_images() {
        let parsed = pack(
            "site",
            "default",
            json!({
                "images": {
                    "cat": { "url": "mxc://hs/1", "body": "a cat", "info": {"mimetype": "image/png"} },
                    "bad!": { "url": "mxc://hs/2" },
                    "dog": { "url": "https://nope" },
                    "ok": { "url": "mxc://hs/3" }
                },
                "pack": {
                    "display_name": "默认包",
                    "usage": ["sticker"],
                    "attribution": "me"
                }
            }),
        )
        .expect("parse")
        .expect("sticker usage");

        assert_eq!(parsed.site_id, "site");
        assert_eq!(parsed.state_key, "default");
        assert_eq!(parsed.content.display_name.as_deref(), Some("默认包"));
        assert_eq!(parsed.content.usage, vec!["sticker"]);
        assert_eq!(
            parsed
                .content
                .images
                .iter()
                .map(|i| i.shortcode.as_str())
                .collect::<Vec<_>>(),
            vec!["cat", "ok"]
        );
        assert_eq!(parsed.content.images[0].body.as_deref(), Some("a cat"));
    }

    #[test]
    fn usage_filters_out_non_sticker_packs() {
        assert_eq!(
            pack(
                "site",
                "emotes",
                json!({"images": {"x": {"url": "mxc://hs/1"}}, "pack": {"usage": ["emoticon"]}})
            )
            .expect("parse"),
            None
        );
        // Absent usage means all usages.
        assert!(
            pack(
                "site",
                "all",
                json!({"images": {"x": {"url": "mxc://hs/1"}}})
            )
            .expect("parse")
            .is_some()
        );
    }

    #[test]
    fn rejects_non_object_and_oversized_content() {
        assert_eq!(
            pack("site", "bad", json!("nope")),
            Err(StickerPackError::NotAnObject)
        );
        let big = json!({
            "images": {
                "x": { "url": "mxc://hs/1" },
                "y": { "url": "mxc://hs/2" },
            },
            "pad": "a".repeat(MAX_PACK_EVENT_BYTES),
        });
        assert_eq!(
            pack("site", "big", big),
            Err(StickerPackError::PackTooLarge)
        );
    }

    #[test]
    fn response_shape_uses_proxy_and_falls_back_to_raw_mxc() {
        let parsed = pack(
            "site",
            "default",
            json!({
                "images": {"cat": {"url": "mxc://hs/1", "body": "cat", "info": {"w": 100}}},
                "pack": {"display_name": "P"}
            }),
        )
        .expect("parse")
        .expect("usage");
        let shape = pack_response_shape(
            &parsed,
            |url| (url == "mxc://hs/1").then(|| "/api/v1/media/hs/1?expires=1".to_string()),
            |_| None,
        );
        assert_eq!(shape["pack_id"], "default");
        assert_eq!(shape["display_name"], "P");
        assert_eq!(shape["images"][0]["shortcode"], "cat");
        assert_eq!(
            shape["images"][0]["proxy_url"],
            "/api/v1/media/hs/1?expires=1"
        );
        assert_eq!(shape["images"][0]["info"]["w"], 100);

        let no_proxy = pack_response_shape(&parsed, |_| None, |_| None);
        assert_eq!(no_proxy["images"][0]["proxy_url"], "mxc://hs/1");
    }

    #[test]
    fn response_shape_proxies_pack_avatar() {
        let parsed = pack(
            "site",
            "default",
            json!({
                "images": {"cat": {"url": "mxc://hs/1"}},
                "pack": {"display_name": "P", "avatar_url": "mxc://hs/avatar"}
            }),
        )
        .expect("parse")
        .expect("usage");
        let shape = pack_response_shape(
            &parsed,
            |_| None,
            |url| (url == "mxc://hs/avatar").then(|| "/api/v1/media/hs/avatar?w=96".to_string()),
        );
        assert_eq!(shape["avatar_url"], "mxc://hs/avatar");
        assert_eq!(shape["avatar_proxy_url"], "/api/v1/media/hs/avatar?w=96");

        let no_proxy = pack_response_shape(&parsed, |_| None, |_| None);
        assert_eq!(no_proxy["avatar_proxy_url"], "mxc://hs/avatar");
    }
}
