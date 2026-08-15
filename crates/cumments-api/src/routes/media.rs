//! Guest media upload, site sticker packs, and the public read-only media
//! proxy for Matrix MXC media.
//!
//! The read model stores `mxc://` references; browsers cannot download them
//! without Matrix credentials. This endpoint fetches the media with the
//! AppService token and streams it back to public readers, using short-lived
//! HMAC-signed URLs to prevent hotlinking.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use crate::request::{IDEMPOTENT_REPLAYED, extract_idempotency_key, request_fingerprint};
use crate::routes::comments::challenge_prefix;
use crate::routes::moderation::rate_limited;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use cumments_core::identity::{signature_message, verify_signature};
use cumments_core::media_upload::MediaUploadIdempotencyInput;
use cumments_core::models::{Content, Message, PostSlug, SiteId};
use cumments_core::site_auth::{constant_time_eq, is_private_ip_addr, sha256_hex};
use cumments_core::sticker_packs::{
    AddStickerInput, StickerPackUseCaseError, add_site_sticker, list_site_sticker_packs,
    pack_response_shape, remove_site_sticker,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::StreamExt;
use tracing::warn;

/// Upper bound on a single proxied media response and on an uploaded file.
///
/// Shared with the write-auth middleware so secret-mode media uploads can be
/// buffered for HMAC verification up to this same bound instead of the much
/// smaller generic request-body limit.
pub(crate) const MEDIA_MAX_BYTES: usize = 20 * 1024 * 1024;
/// How long a signed media URL stays valid.
const MEDIA_URL_TTL_SECONDS: i64 = 15 * 60;
/// Mime prefixes allowed for guest uploads (image, video, audio, files).
const ALLOWED_UPLOAD_MIMES: [&str; 4] = ["image/", "video/", "audio/", "application/"];
/// Content types allowed through the proxy (prefix match).
const ALLOWED_MEDIA_TYPES: [&str; 5] = [
    "image/",
    "video/",
    "audio/",
    "application/pdf",
    "application/octet-stream",
];
/// CSP applied to proxied SVG documents: they render (inline styles and
/// `data:` images) but cannot execute scripts, even when opened directly as a
/// top-level document in the Cumments origin.
const SVG_CSP: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:";

type HmacSha256 = Hmac<Sha256>;

/// Server-side media proxy configuration.
pub struct MediaProxy {
    homeserver_url: String,
    as_token: String,
    /// HMAC key for signed media URLs, independent of `as_token` so AS token
    /// rotation does not invalidate outstanding proxy URLs.
    sign_key: String,
    /// Allow fetching loopback/private/link-local media servers. Off by
    /// default because the proxy is an SSRF surface.
    allow_private_servers: bool,
    http_client: reqwest::Client,
}

impl MediaProxy {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        sign_key: String,
        allow_private_servers: bool,
    ) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            homeserver_url,
            as_token,
            sign_key,
            allow_private_servers,
            http_client,
        })
    }

    /// Converts an `mxc://server/media_id` reference into a short-lived
    /// signed proxy URL. Any syntactically valid, policy-allowed server is
    /// proxied; `None` is returned for malformed or blocked servers.
    pub fn proxify(&self, url: &str) -> Option<String> {
        self.proxify_inner(url, false)
    }

    /// Like [`Self::proxify`], but requests the homeserver's 320×320
    /// thumbnail variant (`thumbnail=1`).
    pub fn proxify_thumbnail(&self, url: &str) -> Option<String> {
        self.proxify_inner(url, true)
    }

    fn proxify_inner(&self, url: &str, thumbnail: bool) -> Option<String> {
        let rest = url.strip_prefix("mxc://")?;
        let (server, media_id) = rest.split_once('/')?;
        if !is_valid_media_server(server) {
            return None;
        }
        if !self.allow_private_servers
            && let Ok(ip) = server.parse::<IpAddr>()
            && is_private_ip_addr(ip)
        {
            return None;
        }
        let expires = now_epoch_seconds() + MEDIA_URL_TTL_SECONDS;
        let signature = self.sign(server, media_id, expires);
        let thumbnail_query = if thumbnail { "&thumbnail=1" } else { "" };
        Some(format!(
            "/api/v1/media/{server}/{media_id}?expires={expires}&sig={signature}{thumbnail_query}"
        ))
    }

    /// Rewrites media URLs inside a message for API/SSE delivery.
    pub fn proxify_message(&self, message: &mut Message) {
        match &mut message.content {
            Content::Media(media) => {
                if let Some(url) = self.proxify(&media.url) {
                    media.url = url;
                }
                if let Some(thumbnail) = media
                    .thumbnail_url
                    .as_deref()
                    .and_then(|u| self.proxify_thumbnail(u))
                {
                    media.thumbnail_url = Some(thumbnail);
                }
            }
            Content::Location(location) => {
                if let Some(thumbnail) = location
                    .thumbnail_url
                    .as_deref()
                    .and_then(|u| self.proxify_thumbnail(u))
                {
                    location.thumbnail_url = Some(thumbnail);
                }
            }
            _ => {}
        }
    }

    /// Verifies an HMAC-signed media URL without revealing the token.
    pub fn verify(&self, server: &str, media_id: &str, expires: i64, signature: &str) -> bool {
        if !is_valid_media_server(server) || media_id.is_empty() || media_id.contains('/') {
            return false;
        }
        if !self.allow_private_servers
            && let Ok(ip) = server.parse::<IpAddr>()
            && is_private_ip_addr(ip)
        {
            return false;
        }
        let now = now_epoch_seconds();
        if expires < now - 60 || expires > now + MEDIA_URL_TTL_SECONDS {
            return false;
        }
        let expected = self.sign(server, media_id, expires);
        constant_time_eq(expected.as_bytes(), signature.as_bytes())
    }

    /// Fetches media from the homeserver as the AppService.
    pub async fn fetch(
        &self,
        server: &str,
        media_id: &str,
        thumbnail: bool,
    ) -> anyhow::Result<reqwest::Response> {
        if !is_valid_media_server(server) || media_id.is_empty() || media_id.contains('/') {
            anyhow::bail!("invalid mxc server/media id in media proxy request");
        }
        if !self.allow_private_servers {
            if let Ok(ip) = server.parse::<IpAddr>() {
                if is_private_ip_addr(ip) {
                    anyhow::bail!("media proxy refuses private server {server}");
                }
            } else {
                let resolver = hickory_resolver::TokioResolver::builder_tokio()
                    .and_then(|builder| builder.build())
                    .map_err(|e| anyhow::anyhow!("failed to initialize DNS resolver: {e}"))?;
                let lookup = resolver.lookup_ip(server.to_string()).await.map_err(|e| {
                    anyhow::anyhow!("failed to resolve media server `{server}`: {e}")
                })?;
                if lookup.iter().any(is_private_ip_addr) {
                    anyhow::bail!(
                        "media proxy refuses server {server}: resolves to a private address"
                    );
                }
            }
        }
        let path = if thumbnail {
            format!("_matrix/media/v3/thumbnail/{server}/{media_id}?width=320&height=320")
        } else {
            format!("_matrix/media/v3/download/{server}/{media_id}")
        };
        let url = format!("{}/{}", self.homeserver_url.trim_end_matches('/'), path);
        Ok(self
            .http_client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.as_token))
            .send()
            .await?)
    }

    fn sign(&self, server: &str, media_id: &str, expires: i64) -> String {
        let mut mac = HmacSha256::new_from_slice(self.sign_key.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(server.as_bytes());
        mac.update(b"/");
        mac.update(media_id.as_bytes());
        mac.update(b"/");
        mac.update(expires.to_string().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// Whether a Matrix server name is syntactically acceptable for media
/// proxying: an IP literal or a DNS hostname.
fn is_valid_media_server(server: &str) -> bool {
    if server.parse::<IpAddr>().is_ok() {
        return true;
    }
    if server.is_empty() || server.len() > 253 || server.starts_with('.') || server.ends_with('.') {
        return false;
    }
    server.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn media_upload_response(
    url: String,
    filename: String,
    mimetype: String,
    size: usize,
    replayed: bool,
) -> Response {
    let kind = if mimetype.starts_with("image/") {
        "image"
    } else if mimetype.starts_with("video/") {
        "video"
    } else if mimetype.starts_with("audio/") {
        "audio"
    } else {
        "file"
    };
    let mut response = (Json(serde_json::json!({
        "url": url,
        "filename": filename,
        "mimetype": mimetype,
        "size": size,
        "voice": false,
        "kind": kind,
    })),)
        .into_response();
    if replayed {
        response.headers_mut().insert(
            IDEMPOTENT_REPLAYED.clone(),
            HeaderValue::from_static("true"),
        );
    }
    response
}

/// Best-effort rollback of a media upload that could not be recorded
/// locally. A failed rollback leaves an untracked orphan on the homeserver;
/// the warning carries the URL so an operator can clean it manually.
async fn rollback_media_upload(state: &ApiState, url: &str) {
    let Some(rest) = url.strip_prefix("mxc://") else {
        return;
    };
    let Some((server, media_id)) = rest.split_once('/') else {
        return;
    };
    if let Err(error) = state.driver.delete_media(server, media_id).await {
        warn!(
            url,
            %error,
            "failed to roll back media upload after local record failure"
        );
    }
}

/// Guest media upload: verifies PoW + author signature, then asks the
/// `MatrixDriver` to upload as the author's virtual user. The driver is the
/// only homeserver write seam.
pub(crate) async fn upload_media_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((site_id, post_slug)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    if state.driver.sender_user_id().is_none() {
        return Err(AppError::NotFound(
            "Media uploads are not enabled for this deployment.".to_string(),
        ));
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let post_slug_val = PostSlug::new(post_slug).map_err(AppError::Validation)?;

    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "media uploads are rate limited; try again later".to_string(),
            retry_after_seconds: state.write_limiter.window().as_secs(),
        });
    }
    if body.len() > MEDIA_MAX_BYTES {
        return Err(AppError::BadRequest(
            "media exceeds the size limit".to_string(),
        ));
    }
    let idempotency_key = extract_idempotency_key(&headers)?;
    let author_public_key = query
        .get("author_public_key")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing author_public_key".to_string()))?;
    let author_signature = query
        .get("author_signature")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing author_signature".to_string()))?;
    let challenge_response = query
        .get("challenge_response")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing challenge_response".to_string()))?;
    let mimetype = query
        .get("mime")
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let filename = query
        .get("filename")
        .cloned()
        .unwrap_or_else(|| "upload".to_string());

    if !ALLOWED_UPLOAD_MIMES
        .iter()
        .any(|allowed| mimetype.starts_with(allowed))
    {
        return Err(AppError::BadRequest(format!(
            "unsupported upload media type {mimetype}"
        )));
    }

    let fingerprint = format!(
        "{}\n{}\n{}",
        request_fingerprint(
            "POST",
            &format!(
                "/api/v1/sites/{}/posts/{}/media",
                site_id_val.as_str(),
                post_slug_val.as_str()
            ),
            &body,
        ),
        mimetype,
        filename,
    );

    // Idempotency replay short-circuits before PoW so a retry does not need
    // a fresh proof of work. The Ed25519 signature is still verified so a
    // guessed key cannot leak someone else's media URL.
    if let Some(existing) = state
        .store
        .find_media_upload_idempotency(&author_public_key, &idempotency_key)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check media idempotency: {e}")))?
    {
        if existing.request_fingerprint != fingerprint {
            return Err(AppError::IdempotencyReused);
        }
        let challenge = challenge_prefix(&challenge_response);
        let message = signature_message(&[
            "UPLOAD",
            site_id_val.as_str(),
            post_slug_val.as_str(),
            &mimetype,
            &filename,
            &sha256_hex(&body),
            challenge,
        ]);
        if !verify_signature(&author_public_key, &message, &author_signature) {
            return Err(AppError::InvalidSignature);
        }
        return Ok(media_upload_response(
            existing.mxc_url,
            filename,
            mimetype,
            body.len(),
            true,
        ));
    }

    if !state.pow.verify(&challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&challenge_response);
    let message = signature_message(&[
        "UPLOAD",
        site_id_val.as_str(),
        post_slug_val.as_str(),
        &mimetype,
        &filename,
        &sha256_hex(&body),
        challenge,
    ]);
    if !verify_signature(&author_public_key, &message, &author_signature) {
        return Err(AppError::InvalidSignature);
    }

    let size = body.len();
    let url = state
        .driver
        .upload_media(body, &filename, &mimetype, &author_public_key, &site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upload media: {e}")))?;
    let outcome = match state
        .store
        .save_media_upload_idempotent(
            &url,
            &author_public_key,
            site_id_val.as_str(),
            post_slug_val.as_str(),
            &MediaUploadIdempotencyInput {
                key: idempotency_key,
                request_fingerprint: fingerprint,
            },
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            rollback_media_upload(&state, &url).await;
            return Err(AppError::Internal(format!(
                "failed to record media upload: {e}"
            )));
        }
    };

    match outcome {
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Created { mxc_url } => Ok(
            media_upload_response(mxc_url, filename, mimetype, size, false),
        ),
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Replayed { mxc_url } => {
            rollback_media_upload(&state, &url).await;
            Ok(media_upload_response(
                mxc_url, filename, mimetype, size, true,
            ))
        }
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Reused => {
            rollback_media_upload(&state, &url).await;
            Err(AppError::IdempotencyReused)
        }
    }
}

/// Lists a site's projected sticker packs for guests.
pub(crate) async fn list_stickers_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let packs = list_site_sticker_packs(state.store.as_ref(), site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to list sticker packs: {e}")))?;
    let packs = packs
        .iter()
        .map(|projection| {
            pack_response_shape(&projection.pack, |url| {
                state
                    .media_proxy
                    .as_ref()
                    .and_then(|proxy| proxy.proxify(url))
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({ "packs": packs })))
}

/// Body of `POST .../packs/{pack_id}/stickers`.
#[derive(Debug, Deserialize)]
pub struct AddStickerRequest {
    pub shortcode: String,
    pub url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub info: Option<serde_json::Value>,
}

/// Adds or replaces one sticker image in a site's pack (site governance,
/// claim-token authenticated; operator fallback uses the same handler).
pub(crate) async fn add_site_sticker_handler(
    State(state): State<ApiState>,
    Path((site_id, pack_id)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AddStickerRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    rate_limited(&state, &headers, Some(connect.0))?;
    let projection = add_site_sticker(
        state.store.as_ref(),
        state.driver.as_ref(),
        AddStickerInput {
            site_id: &site_id,
            pack_id: &pack_id,
            shortcode: &req.shortcode,
            url: &req.url,
            body: req.body,
            info: req.info,
        },
    )
    .await
    .map_err(map_sticker_use_case_error)?;
    Ok(Json(pack_response_shape(&projection.pack, |url| {
        state
            .media_proxy
            .as_ref()
            .and_then(|proxy| proxy.proxify(url))
    })))
}

/// Removes one sticker image from a site's pack.
pub(crate) async fn remove_site_sticker_handler(
    State(state): State<ApiState>,
    Path((site_id, pack_id)): Path<(String, String)>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    rate_limited(&state, &headers, Some(connect.0))?;
    let shortcode = query
        .get("shortcode")
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| AppError::BadRequest("shortcode query parameter is required".to_string()))?;
    let projection = remove_site_sticker(
        state.store.as_ref(),
        state.driver.as_ref(),
        &site_id,
        &pack_id,
        &shortcode,
    )
    .await
    .map_err(map_sticker_use_case_error)?;
    Ok(Json(pack_response_shape(&projection.pack, |url| {
        state
            .media_proxy
            .as_ref()
            .and_then(|proxy| proxy.proxify(url))
    })))
}

fn map_sticker_use_case_error(error: StickerPackUseCaseError) -> AppError {
    match error {
        StickerPackUseCaseError::Invalid(message) => AppError::BadRequest(message.to_string()),
        StickerPackUseCaseError::SiteNotFound(site_id) => {
            AppError::NotFound(format!("site {site_id} not found"))
        }
        StickerPackUseCaseError::SiteWithoutSpace(site_id) => {
            AppError::NotFound(format!("site {site_id} has no Matrix Space"))
        }
        StickerPackUseCaseError::PackNotFound(pack_id) => {
            AppError::NotFound(format!("sticker pack {pack_id} not found"))
        }
        StickerPackUseCaseError::Other(error) => {
            AppError::Internal(format!("sticker pack operation failed: {error}"))
        }
    }
}

/// Serves one media file to a public reader.
pub(crate) async fn media_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((server, media_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let Some(proxy) = &state.media_proxy else {
        return Err(AppError::NotFound(
            "Media proxy is not enabled for this deployment.".to_string(),
        ));
    };

    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.media_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "media requests are rate limited; try again later".to_string(),
            retry_after_seconds: state.media_limiter.window().as_secs(),
        });
    }

    let expires = query
        .get("expires")
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or_else(|| AppError::BadRequest("missing or invalid expires".to_string()))?;
    let signature = query
        .get("sig")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing signature".to_string()))?;
    if !proxy.verify(&server, &media_id, expires, &signature) {
        return Err(AppError::BadRequest(
            "invalid or expired media URL".to_string(),
        ));
    }

    let thumbnail = query.get("thumbnail").map(|v| v == "1").unwrap_or(false);
    let upstream = proxy
        .fetch(&server, &media_id, thumbnail)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch media: {e}")))?;
    if !upstream.status().is_success() {
        return Err(AppError::NotFound("Media not found.".to_string()));
    }

    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !ALLOWED_MEDIA_TYPES
        .iter()
        .any(|allowed| content_type.starts_with(allowed))
    {
        return Err(AppError::BadRequest(format!(
            "unsupported media type {content_type}"
        )));
    }

    // Reject obviously oversized responses from the header before reading, so
    // the 400 is a proper error envelope rather than a truncated stream.
    if upstream
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > MEDIA_MAX_BYTES)
    {
        return Err(AppError::BadRequest(
            "media exceeds the size limit".to_string(),
        ));
    }

    // Read in bounded chunks so the memory used is capped at the size limit
    // even when the upstream omits Content-Length.
    let mut bytes = Vec::new();
    let mut stream = upstream.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::Internal(format!("failed to read media: {e}")))?;
        if bytes.len() + chunk.len() > MEDIA_MAX_BYTES {
            return Err(AppError::BadRequest(
                "media exceeds the size limit".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", media_id),
        )
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .expect("static response builds");
    if is_svg_content_type(&content_type) {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SVG_CSP),
        );
    }
    Ok(response)
}

/// Whether a media content type is an SVG document. Parameters (e.g.
/// `image/svg+xml; charset=utf-8`) are ignored, matching how the upstream
/// content-type is normally emitted.
fn is_svg_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/svg+xml"))
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cumments_core::models::{
        AuthorKind, AuthorSnapshot, MediaContent, MediaKind, Message, MessageStatus, TextContent,
        TextStyle,
    };

    fn proxy() -> MediaProxy {
        MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            false,
        )
        .expect("build proxy")
    }

    fn query_params(url: &str) -> HashMap<String, String> {
        let (_, query) = url.split_once('?').expect("query string");
        query
            .split('&')
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect()
    }

    #[test]
    fn proxify_rewrites_mxc_urls_and_verify_round_trips() {
        let p = proxy();
        let url = p.proxify("mxc://hs/abc").expect("proxify");
        assert!(url.starts_with("/api/v1/media/hs/abc?expires="));
        let params = query_params(&url);
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(p.verify("hs", "abc", expires, &params["sig"]));
    }

    #[test]
    fn proxify_serves_foreign_servers() {
        let p = proxy();
        let url = p.proxify("mxc://other/abc").expect("foreign proxify");
        assert!(url.starts_with("/api/v1/media/other/abc?expires="));
        let params = query_params(&url);
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(
            p.verify("other", "abc", expires, &params["sig"]),
            "foreign-server URLs must verify"
        );
    }

    #[test]
    fn proxify_rejects_private_and_malformed_servers() {
        let p = proxy();
        assert!(p.proxify("mxc://127.0.0.1/abc").is_none());
        assert!(p.proxify("mxc://10.0.0.1/abc").is_none());
        assert!(p.proxify("mxc://::1/abc").is_none());
        assert!(p.proxify("mxc://bad server/abc").is_none());
        assert!(p.proxify("mxc://-bad/abc").is_none());
        assert!(p.proxify("mxc://bad-/abc").is_none());

        let open = MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            true,
        )
        .expect("build open proxy");
        assert!(open.proxify("mxc://127.0.0.1/abc").is_some());
    }

    #[test]
    fn verify_rejects_tampered_and_expired_urls() {
        let p = proxy();
        let params = query_params(&p.proxify("mxc://hs/abc").expect("proxify"));
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(!p.verify("hs", "abc", expires, "deadbeef"));
        assert!(!p.verify("hs", "abc", expires - 1000, &params["sig"]));
        // The signature covers the server, so a URL minted for another
        // server cannot be replayed against it.
        assert!(!p.verify("other", "abc", expires, &params["sig"]));
        assert!(!p.verify("hs", "abc", expires, "not-a-signature"));
    }

    #[test]
    fn proxify_message_rewrites_media_and_thumbnail_urls() {
        let p = proxy();
        let mut message = Message {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            post_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Matrix,
                display_name: None,
                avatar_url: None,
                public_key: None,
                mxid: Some("@alice:hs".to_string()),
            },
            content: Content::Media(MediaContent {
                kind: MediaKind::Image,
                url: "mxc://hs/abc".to_string(),
                filename: Some("cat.png".to_string()),
                mimetype: Some("image/png".to_string()),
                size: None,
                width: None,
                height: None,
                thumbnail_url: Some("mxc://hs/thumb".to_string()),
                alt_text: None,
                voice: false,
            }),
            timestamp: chrono::Utc::now(),
            edited_at: None,
            reply_to: None,
            thread_root: None,
            submission_id: None,
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: Vec::new(),
            room_id: "!room:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            raw_content: serde_json::Value::Null,
        };
        p.proxify_message(&mut message);
        match message.content {
            Content::Media(media) => {
                assert!(media.url.starts_with("/api/v1/media/hs/abc?"));
                let thumbnail = media.thumbnail_url.expect("thumbnail rewritten");
                assert!(
                    thumbnail.starts_with("/api/v1/media/hs/thumb?")
                        && thumbnail.contains("&thumbnail=1"),
                    "thumbnail URL must request the thumbnail variant: {thumbnail}"
                );
            }
            other => panic!("expected media, got {other:?}"),
        }
    }

    #[test]
    fn text_content_is_not_rewritten() {
        let p = proxy();
        let mut message = Message {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            post_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Matrix,
                display_name: None,
                avatar_url: None,
                public_key: None,
                mxid: Some("@alice:hs".to_string()),
            },
            content: Content::Text(TextContent {
                body: "hi".to_string(),
                formatted_body: None,
                style: TextStyle::Normal,
            }),
            timestamp: chrono::Utc::now(),
            edited_at: None,
            reply_to: None,
            thread_root: None,
            submission_id: None,
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: Vec::new(),
            room_id: "!room:hs".to_string(),
            sender_mxid: "@alice:hs".to_string(),
            raw_content: serde_json::Value::Null,
        };
        p.proxify_message(&mut message);
        assert!(matches!(message.content, Content::Text(_)));
    }

    #[test]
    fn svg_content_type_detection_handles_parameters_and_case() {
        assert!(is_svg_content_type("image/svg+xml"));
        assert!(is_svg_content_type("image/svg+xml; charset=utf-8"));
        assert!(is_svg_content_type("IMAGE/SVG+XML"));
        assert!(!is_svg_content_type("image/png"));
        assert!(!is_svg_content_type("text/html"));
    }
}
