//! Visitor media upload, site sticker packs, and the public read-only media
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
use crate::routes::governance::rate_limited;
use crate::trusted_proxy::TrustedProxySet;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use cumments_core::identity::{signature_message, verify_signature};
use cumments_core::media_upload::MediaUploadIdempotencyInput;
use cumments_core::models::{Content, Message, PageSlug, SiteId};
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
use url::Url;

/// Upper bound on a single proxied media response and on an uploaded file.
///
/// Shared with the write-auth middleware so secret-mode media uploads can be
/// buffered for HMAC verification up to this same bound instead of the much
/// smaller generic request-body limit.
pub(crate) const MEDIA_MAX_BYTES: usize = 20 * 1024 * 1024;
/// How long a signed media URL stays valid.
const MEDIA_URL_TTL_SECONDS: i64 = 15 * 60;
/// Mime prefixes allowed for visitor uploads (image, video, audio, files).
const ALLOWED_UPLOAD_MIMES: [&str; 4] = ["image/", "video/", "audio/", "application/"];
/// Maximum opaque media-id length accepted by the proxy. This is an
/// operational cap aligned with other Matrix identifier limits; it prevents
/// unbounded path/header/signing input.
const MAX_MEDIA_ID_BYTES: usize = 255;
/// Exact MIME types Matrix identifies as safe to serve inline.
const INLINE_CONTENT_TYPES: [&str; 26] = [
    "text/css",
    "text/plain",
    "text/csv",
    "application/json",
    "application/ld+json",
    "image/jpeg",
    "image/gif",
    "image/png",
    "image/apng",
    "image/webp",
    "image/avif",
    "video/mp4",
    "video/webm",
    "video/ogg",
    "video/quicktime",
    "audio/mp4",
    "audio/webm",
    "audio/aac",
    "audio/mpeg",
    "audio/ogg",
    "audio/wave",
    "audio/wav",
    "audio/x-wav",
    "audio/x-pn-wav",
    "audio/flac",
    "audio/x-flac",
];
/// Thumbnail responses are a small, browser-safe image set per the Matrix
/// content-repository specification.
const THUMBNAIL_CONTENT_TYPES: [&str; 5] = [
    "image/jpeg",
    "image/png",
    "image/apng",
    "image/gif",
    "image/webp",
];
/// Default thumbnail requested for avatars: the spec's recommended avatar
/// bucket and the size Element-family clients converge on for square avatars.
const AVATAR_THUMBNAIL: ThumbParams = ThumbParams {
    width: 96,
    height: 96,
    method: Some(ThumbMethod::Crop),
};
/// Default thumbnail requested for message/location thumbnails, matching the
/// spec's recommended 320×240 `scale` bucket.
const CONTENT_THUMBNAIL: ThumbParams = ThumbParams {
    width: 320,
    height: 240,
    method: Some(ThumbMethod::Scale),
};
/// Upper bound for requested thumbnail dimensions; the homeserver must never
/// upscale, so oversized requests are rejected before they reach it.
const MAX_THUMBNAIL_DIMENSION: u32 = 4096;
/// CSP applied to every proxied media response. This matches the Matrix
/// recommendation: content is sandboxed into a unique origin, scripts cannot
/// run, and only benign inline styling / PDF plugin behavior is retained.
const MEDIA_CONTENT_SECURITY_POLICY: &str = "sandbox; default-src 'none'; script-src 'none'; \
     plugin-types application/pdf; style-src 'unsafe-inline'; object-src 'self';";

/// How a successfully fetched upstream payload may be presented.
struct MediaPresentation {
    /// Canonical MIME type sent to the browser (upstream parameters removed).
    content_type: String,
    disposition: &'static str,
    filename: String,
}

type HmacSha256 = Hmac<Sha256>;

/// A validated thumbnail request, mirroring the Matrix thumbnail endpoint's
/// width/height/method semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbParams {
    pub width: u32,
    pub height: u32,
    pub method: Option<ThumbMethod>,
}

impl ThumbParams {
    /// Returns `None` when the requested dimensions are outside the proxy's
    /// bounds (the homeserver must never upscale, so oversized requests are
    /// pointless).
    pub fn new(width: u32, height: u32, method: Option<ThumbMethod>) -> Option<Self> {
        (width > 0
            && width <= MAX_THUMBNAIL_DIMENSION
            && height > 0
            && height <= MAX_THUMBNAIL_DIMENSION)
            .then_some(Self {
                width,
                height,
                method,
            })
    }
}

/// The two thumbnail resizing methods defined by the Matrix specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbMethod {
    Crop,
    Scale,
}

impl ThumbMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            ThumbMethod::Crop => "crop",
            ThumbMethod::Scale => "scale",
        }
    }
}

/// Server-side media proxy configuration.
pub struct MediaProxy {
    homeserver_url: String,
    as_token: String,
    /// HMAC key for signed media URLs, independent of `as_token` so AS token
    /// rotation does not invalidate outstanding proxy URLs.
    sign_key: String,
    /// Externally reachable base URL of this API. When set it wins over
    /// request-derived bases; when unset the request's own `Host`/forwarded
    /// headers are used so pages on other origins still get absolute URLs.
    public_base_url: Option<String>,
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
        public_base_url: Option<String>,
        allow_private_servers: bool,
    ) -> anyhow::Result<Self> {
        if let Some(url) = &public_base_url {
            let trimmed = url.trim_end_matches('/');
            let parsed = Url::parse(trimmed).map_err(|error| {
                anyhow::anyhow!("server.public_base_url is not a valid URL: {error}")
            })?;
            anyhow::ensure!(
                parsed.scheme() == "http" || parsed.scheme() == "https",
                "server.public_base_url must start with http:// or https://"
            );
            anyhow::ensure!(
                parsed.host_str().is_some(),
                "server.public_base_url must include a host"
            );
            anyhow::ensure!(
                parsed.username().is_empty() && parsed.password().is_none(),
                "server.public_base_url must not include userinfo"
            );
            anyhow::ensure!(
                parsed.query().is_none() && parsed.fragment().is_none(),
                "server.public_base_url must not include a query or fragment"
            );
        }
        let public_base_url = public_base_url.map(|url| url.trim_end_matches('/').to_owned());
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            homeserver_url,
            as_token,
            sign_key,
            public_base_url,
            allow_private_servers,
            http_client,
        })
    }

    /// Resolves the base URL used to make proxy URLs absolute: the explicit
    /// `server.public_base_url` wins; otherwise the base is derived from the
    /// request so the returned URLs are reachable by that client.
    pub fn public_base(
        &self,
        headers: &HeaderMap,
        addr: Option<SocketAddr>,
        trusted_proxies: &TrustedProxySet,
    ) -> Option<String> {
        match &self.public_base_url {
            Some(base) => Some(base.clone()),
            None => request_public_base(headers, addr, trusted_proxies),
        }
    }

    /// Converts an `mxc://server/media_id` reference into a short-lived
    /// signed proxy URL. Any syntactically valid, policy-allowed server is
    /// proxied; `None` is returned for malformed or blocked servers.
    pub fn proxify(&self, url: &str, base: &str) -> Option<String> {
        self.proxify_sized(url, None, base)
    }

    /// Like [`Self::proxify`], but requests the homeserver's default
    /// content-thumbnail variant (320×240, `scale`).
    pub fn proxify_thumbnail(&self, url: &str, base: &str) -> Option<String> {
        self.proxify_sized(url, Some(CONTENT_THUMBNAIL), base)
    }

    /// Like [`Self::proxify`], but requests the default avatar thumbnail
    /// variant (96×96, `crop`).
    pub fn proxify_avatar(&self, url: &str, base: &str) -> Option<String> {
        self.proxify_sized(url, Some(AVATAR_THUMBNAIL), base)
    }

    /// Mints a signed proxy URL. Thumbnail dimensions and method, when
    /// present, are part of the signature so a small-avatar URL cannot be
    /// rewritten into a large/arbitrary thumbnail request.
    fn proxify_sized(&self, url: &str, thumb: Option<ThumbParams>, base: &str) -> Option<String> {
        let rest = url.strip_prefix("mxc://")?;
        let (server, media_id) = rest.split_once('/')?;
        if !is_valid_media_server(server) {
            return None;
        }
        if !is_valid_media_id(media_id) {
            return None;
        }
        if !self.allow_private_servers
            && let Ok(ip) = server.parse::<IpAddr>()
            && is_private_ip_addr(ip)
        {
            return None;
        }
        let expires = now_epoch_seconds() + MEDIA_URL_TTL_SECONDS;
        let signature = self.sign(server, media_id, thumb, expires);
        let mut query = format!("?expires={expires}&sig={signature}");
        if let Some(thumb) = thumb {
            query.push_str(&format!("&width={}&height={}", thumb.width, thumb.height));
            if let Some(method) = thumb.method {
                query.push_str(&format!("&method={}", method.as_str()));
            }
        }
        let path = format!("/api/v1/media/{server}/{media_id}{query}");
        if base.is_empty() {
            Some(path)
        } else {
            Some(format!("{}{path}", base.trim_end_matches('/')))
        }
    }

    /// Rewrites media and author-avatar URLs inside a message for API/SSE
    /// delivery.
    pub fn proxify_message(&self, message: &mut Message, base: &str) {
        match &mut message.content {
            Content::Media(media) => {
                if let Some(url) = self.proxify(&media.url, base) {
                    media.url = url;
                }
                if let Some(thumbnail) = media
                    .thumbnail_url
                    .as_deref()
                    .and_then(|u| self.proxify_thumbnail(u, base))
                {
                    media.thumbnail_url = Some(thumbnail);
                }
            }
            Content::Location(location) => {
                if let Some(thumbnail) = location
                    .thumbnail_url
                    .as_deref()
                    .and_then(|u| self.proxify_thumbnail(u, base))
                {
                    location.thumbnail_url = Some(thumbnail);
                }
            }
            _ => {}
        }
        if let Some(avatar) = message
            .author
            .avatar_url
            .as_deref()
            .and_then(|url| self.proxify_avatar(url, base))
        {
            message.author.avatar_url = Some(avatar);
        }
    }

    /// Verifies an HMAC-signed media URL without revealing the token.
    pub fn verify(
        &self,
        server: &str,
        media_id: &str,
        thumb: Option<ThumbParams>,
        expires: i64,
        signature: &str,
    ) -> bool {
        if !is_valid_media_server(server) || !is_valid_media_id(media_id) {
            return false;
        }
        let thumb = match thumb {
            Some(thumb) => {
                let Some(thumb) = ThumbParams::new(thumb.width, thumb.height, thumb.method) else {
                    return false;
                };
                Some(thumb)
            }
            None => None,
        };
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
        let expected = self.sign(server, media_id, thumb, expires);
        constant_time_eq(expected.as_bytes(), signature.as_bytes())
    }

    /// Fetches media from the homeserver as the AppService, using the
    /// authenticated v1 endpoints (MSC3916). The legacy unauthenticated
    /// `/_matrix/media/v3/*` endpoints are frozen and disabled by default in
    /// tuwunel, so they are not used.
    pub async fn fetch(
        &self,
        server: &str,
        media_id: &str,
        thumb: Option<ThumbParams>,
    ) -> anyhow::Result<reqwest::Response> {
        if !is_valid_media_server(server) || !is_valid_media_id(media_id) {
            anyhow::bail!("invalid mxc server/media id in media proxy request");
        }
        if let Some(thumb) = thumb {
            let Some(_) = ThumbParams::new(thumb.width, thumb.height, thumb.method) else {
                anyhow::bail!("invalid thumbnail dimensions or method");
            };
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
                let first = resolver.lookup_ip(server.to_string()).await.map_err(|e| {
                    anyhow::anyhow!("failed to resolve media server `{server}`: {e}")
                })?;
                if first.iter().any(is_private_ip_addr) {
                    anyhow::bail!(
                        "media proxy refuses server {server}: resolves to a private address"
                    );
                }
                // The proxy never connects to `server` itself: it delegates
                // the fetch to the configured homeserver, which performs its
                // own resolution for federated media. A second independent
                // resolution that disagrees (or leaks a private address) is
                // a DNS-rebinding attempt and is refused; the homeserver's
                // own remote-media protections remain the final boundary.
                let second = resolver.lookup_ip(server.to_string()).await.map_err(|e| {
                    anyhow::anyhow!("failed to re-resolve media server `{server}`: {e}")
                })?;
                if second.iter().any(is_private_ip_addr) {
                    anyhow::bail!(
                        "media proxy refuses server {server}: resolves to a private address"
                    );
                }
                let mut first_ips = first.iter().collect::<Vec<_>>();
                first_ips.sort();
                let mut second_ips = second.iter().collect::<Vec<_>>();
                second_ips.sort();
                if first_ips != second_ips {
                    anyhow::bail!("media proxy refuses server {server}: DNS answers are unstable");
                }
            }
        }
        let path = upstream_media_path(server, media_id, thumb);
        let url = format!("{}/{}", self.homeserver_url.trim_end_matches('/'), path);
        Ok(self
            .http_client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.as_token))
            .send()
            .await?)
    }

    fn sign(
        &self,
        server: &str,
        media_id: &str,
        thumb: Option<ThumbParams>,
        expires: i64,
    ) -> String {
        let mut mac = HmacSha256::new_from_slice(self.sign_key.as_bytes())
            .expect("hmac accepts any key length");
        let (width, height, method) = match thumb {
            Some(thumb) => (
                thumb.width,
                thumb.height,
                thumb.method.map_or("", ThumbMethod::as_str),
            ),
            None => (0, 0, ""),
        };
        mac.update(server.as_bytes());
        mac.update(b"/");
        mac.update(media_id.as_bytes());
        mac.update(b"/");
        mac.update(width.to_string().as_bytes());
        mac.update(b"/");
        mac.update(height.to_string().as_bytes());
        mac.update(b"/");
        mac.update(method.as_bytes());
        mac.update(b"/");
        mac.update(expires.to_string().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// Resolves the media proxy URL base for one request. An empty result means
/// the caller should fall back to API-relative proxy URLs.
pub(crate) fn media_url_base(
    state: &ApiState,
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
) -> String {
    state
        .media_proxy
        .as_ref()
        .and_then(|proxy| proxy.public_base(headers, addr, &state.trusted_proxies))
        .unwrap_or_default()
}

/// Derives the externally reachable API base from the request. A trusted
/// reverse proxy may override the scheme and host via `X-Forwarded-Proto` /
/// `X-Forwarded-Host`; otherwise the connection's own `Host` header is used
/// with `http` (TLS is terminated by the proxy, not by Cumments).
fn request_public_base(
    headers: &HeaderMap,
    addr: Option<SocketAddr>,
    trusted_proxies: &TrustedProxySet,
) -> Option<String> {
    let trusted_peer = addr.is_some_and(|addr| trusted_proxies.contains(addr.ip()));
    let scheme = if trusted_peer {
        first_header_value(headers, "x-forwarded-proto").unwrap_or_else(|| "http".to_string())
    } else {
        "http".to_string()
    };
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = if trusted_peer {
        first_header_value(headers, "x-forwarded-host")
            .or_else(|| first_header_value(headers, "host"))
    } else {
        first_header_value(headers, "host")
    }?;
    let candidate = format!("{scheme}://{host}");
    let parsed = Url::parse(&candidate).ok()?;
    if parsed.host_str().is_none()
        || parsed.path() != "/"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(candidate.trim_end_matches('/').to_string())
}

/// Reads the first comma-separated value of a header, trimmed.
fn first_header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    value
        .split(',')
        .next()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
}

/// Builds the homeserver-relative path for a proxied media request, mapping
/// the proxy's signed thumbnail parameters onto the authenticated v1 media
/// endpoints.
fn upstream_media_path(server: &str, media_id: &str, thumb: Option<ThumbParams>) -> String {
    let Some(thumb) = thumb else {
        return format!("_matrix/client/v1/media/download/{server}/{media_id}");
    };
    let mut path = format!(
        "_matrix/client/v1/media/thumbnail/{server}/{media_id}?width={}&height={}",
        thumb.width, thumb.height
    );
    if let Some(method) = thumb.method {
        path.push_str(&format!("&method={}", method.as_str()));
    }
    path
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

/// Whether an opaque Matrix media-id is safe for signing, upstream requests,
/// and URL construction. This is the whitelist required by the Matrix content
/// repository security rules: `A-Za-z0-9_-`.
fn is_valid_media_id(media_id: &str) -> bool {
    !media_id.is_empty()
        && media_id.len() <= MAX_MEDIA_ID_BYTES
        && media_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Parses and classifies an upstream Content-Type.
///
/// Upstream parameters are intentionally discarded. Only a canonical MIME type
/// is forwarded to browsers. Safe types are served inline per the Matrix allow
/// list; other accepted media (including uncommon image/video/audio codecs,
/// SVG, PDF, and opaque data) are forced to attachment.
fn classify_media_content_type(raw: &str, thumbnail: bool) -> Option<MediaPresentation> {
    let content_type = raw.split(';').next()?.trim().to_ascii_lowercase();
    if content_type.is_empty() {
        return None;
    }

    let (disposition, filename) = if thumbnail {
        if !THUMBNAIL_CONTENT_TYPES.contains(&content_type.as_str()) {
            return None;
        }
        ("inline", "thumbnail")
    } else if INLINE_CONTENT_TYPES.contains(&content_type.as_str()) {
        ("inline", "media")
    } else if content_type.starts_with("image/")
        || content_type.starts_with("video/")
        || content_type.starts_with("audio/")
        || matches!(
            content_type.as_str(),
            "application/pdf" | "application/octet-stream"
        )
    {
        ("attachment", "download")
    } else {
        return None;
    };

    let extension = media_file_extension(&content_type);
    Some(MediaPresentation {
        content_type,
        disposition,
        filename: format!("{filename}.{extension}"),
    })
}

fn media_file_extension(content_type: &str) -> &'static str {
    match content_type {
        "text/css" => "css",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/json" | "application/ld+json" => "json",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/apng" => "apng",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/svg+xml" => "svg",
        "video/mp4" | "audio/mp4" => "mp4",
        "video/webm" | "audio/webm" => "webm",
        "video/ogg" | "audio/ogg" => "ogg",
        "video/quicktime" => "mov",
        "audio/aac" => "aac",
        "audio/mpeg" => "mp3",
        "audio/wave" | "audio/wav" | "audio/x-wav" | "audio/x-pn-wav" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "application/pdf" => "pdf",
        _ => "bin",
    }
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

/// Visitor media upload: verifies PoW + author signature, then asks the
/// `MatrixDriver` to upload as the author's virtual user. The driver is the
/// only homeserver write seam.
pub(crate) async fn upload_media_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((site_id, page_slug)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    if state.driver.sender_user_id().is_none() {
        return Err(AppError::NotFound(
            "Media uploads are not enabled for this deployment.".to_string(),
        ));
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let page_slug_val = PageSlug::new(page_slug).map_err(AppError::Validation)?;

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
                "/api/v1/sites/{}/pages/{}/media",
                site_id_val.as_str(),
                page_slug_val.as_str()
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
        let body_hash = sha256_hex(&body);
        let message = signature_message(&[
            Some("UPLOAD"),
            Some(site_id_val.as_str()),
            Some(page_slug_val.as_str()),
            Some(mimetype.as_str()),
            Some(filename.as_str()),
            Some(body_hash.as_str()),
            Some(challenge),
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
    let body_hash2 = sha256_hex(&body);
    let message = signature_message(&[
        Some("UPLOAD"),
        Some(site_id_val.as_str()),
        Some(page_slug_val.as_str()),
        Some(mimetype.as_str()),
        Some(filename.as_str()),
        Some(body_hash2.as_str()),
        Some(challenge),
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
            Some(page_slug_val.as_str()),
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

/// Builds the JSON response for a visitor avatar write, marking idempotent
/// replays with the same header as media uploads.
fn avatar_response(avatar_url: &str, proxied_url: Option<String>, replayed: bool) -> Response {
    let mut response = (Json(serde_json::json!({
        "avatar_url": proxied_url.unwrap_or_else(|| avatar_url.to_owned()),
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

/// Visitor avatar upload: verifies PoW + author signature, uploads the image as
/// the author's virtual user, records the site-scoped upload idempotently,
/// then points the virtual user's global profile at it.
///
/// `UPLOAD_AVATAR` is a one-request operation (upload + set profile); the
/// media upload machinery is shared with comment media so ownership,
/// idempotency, rate limiting and cleanup behave identically.
pub(crate) async fn set_visitor_avatar_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, AppError> {
    if state.driver.sender_user_id().is_none() {
        return Err(AppError::NotFound(
            "Avatar uploads are not enabled for this deployment.".to_string(),
        ));
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;
    let media_base = media_url_base(&state, &headers, Some(addr));
    let proxied_avatar = |mxc_url: &str| {
        state
            .media_proxy
            .as_ref()
            .and_then(|proxy| proxy.proxify_avatar(mxc_url, &media_base))
    };

    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "avatar uploads are rate limited; try again later".to_string(),
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
        .unwrap_or_else(|| "avatar".to_string());

    if !mimetype.starts_with("image/") {
        return Err(AppError::BadRequest(format!(
            "avatar must be an image, got {mimetype}"
        )));
    }

    let fingerprint = format!(
        "{}\n{}\n{}",
        request_fingerprint(
            "PUT",
            &format!("/api/v1/sites/{}/visitors/avatar", site_id_val.as_str()),
            &body,
        ),
        mimetype,
        filename,
    );
    let sign_message = |challenge: &str| {
        let body_hash = sha256_hex(&body);
        signature_message(&[
            Some("UPLOAD_AVATAR"),
            Some(site_id_val.as_str()),
            Some(mimetype.as_str()),
            Some(body_hash.as_str()),
            Some(challenge),
        ])
    };

    // Idempotency replay short-circuits before PoW so a retry does not need
    // a fresh proof of work; the Ed25519 signature is still verified. The
    // profile write is repeated so a retry heals a partially-completed set.
    if let Some(existing) = state
        .store
        .find_media_upload_idempotency(&author_public_key, &idempotency_key)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check avatar idempotency: {e}")))?
    {
        if existing.request_fingerprint != fingerprint {
            return Err(AppError::IdempotencyReused);
        }
        let challenge = challenge_prefix(&challenge_response);
        if !verify_signature(
            &author_public_key,
            &sign_message(challenge),
            &author_signature,
        ) {
            return Err(AppError::InvalidSignature);
        }
        state
            .driver
            .set_avatar_url(&author_public_key, &site_id_val, Some(&existing.mxc_url))
            .await
            .map_err(|e| AppError::Internal(format!("failed to set avatar: {e}")))?;
        return Ok(avatar_response(
            &existing.mxc_url,
            proxied_avatar(&existing.mxc_url),
            true,
        ));
    }

    if !state.pow.verify(&challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&challenge_response);
    if !verify_signature(
        &author_public_key,
        &sign_message(challenge),
        &author_signature,
    ) {
        return Err(AppError::InvalidSignature);
    }

    let url = state
        .driver
        .upload_media(body, &filename, &mimetype, &author_public_key, &site_id_val)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upload avatar media: {e}")))?;
    let outcome = match state
        .store
        .save_media_upload_idempotent(
            &url,
            &author_public_key,
            site_id_val.as_str(),
            None,
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
                "failed to record avatar upload: {e}"
            )));
        }
    };

    match outcome {
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Created { mxc_url } => {
            if let Err(e) = state
                .driver
                .set_avatar_url(&author_public_key, &site_id_val, Some(&mxc_url))
                .await
            {
                // Undo both sides so a retry with the same key re-uploads
                // cleanly instead of pointing the profile at a rolled-back
                // or missing media copy.
                rollback_media_upload(&state, &mxc_url).await;
                if let Err(cleanup) = state.store.delete_media_upload(&mxc_url).await {
                    warn!(
                        url = mxc_url,
                        %cleanup,
                        "failed to clean up avatar upload record after profile write failure"
                    );
                }
                return Err(AppError::Internal(format!("failed to set avatar: {e}")));
            }
            // The avatar is referenced by the virtual user's profile, so it
            // must never be collected by the unused-media sweep. A failure
            // to mark it rolls the upload back so a retry with the same key
            // re-uploads cleanly instead of leaving an unmarked orphan.
            if let Err(e) = state.store.mark_media_used(&mxc_url).await {
                rollback_media_upload(&state, &mxc_url).await;
                if let Err(cleanup) = state.store.delete_media_upload(&mxc_url).await {
                    warn!(
                        url = mxc_url,
                        %cleanup,
                        "failed to clean up avatar upload record after reference-marking failure"
                    );
                }
                return Err(AppError::Internal(format!(
                    "failed to mark avatar media as referenced: {e}"
                )));
            }
            Ok(avatar_response(&mxc_url, proxied_avatar(&mxc_url), false))
        }
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Replayed { mxc_url } => {
            rollback_media_upload(&state, &url).await;
            state
                .driver
                .set_avatar_url(&author_public_key, &site_id_val, Some(&mxc_url))
                .await
                .map_err(|e| AppError::Internal(format!("failed to set avatar: {e}")))?;
            Ok(avatar_response(&mxc_url, proxied_avatar(&mxc_url), true))
        }
        cumments_core::media_upload::MediaUploadIdempotencyOutcome::Reused => {
            rollback_media_upload(&state, &url).await;
            Err(AppError::IdempotencyReused)
        }
    }
}

/// Removes the visitor's avatar: verifies PoW + author signature, then deletes
/// the virtual user's `avatar_url` profile field. Deleting a missing avatar
/// is a successful no-op on the homeserver side.
pub(crate) async fn delete_visitor_avatar_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if state.driver.sender_user_id().is_none() {
        return Err(AppError::NotFound(
            "Avatar management is not enabled for this deployment.".to_string(),
        ));
    }
    let site_id_val = SiteId::new(site_id).map_err(AppError::Validation)?;

    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.write_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "avatar deletes are rate limited; try again later".to_string(),
            retry_after_seconds: state.write_limiter.window().as_secs(),
        });
    }
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

    if !state.pow.verify(&challenge_response) {
        return Err(AppError::InvalidPoW);
    }
    let challenge = challenge_prefix(&challenge_response);
    let message = signature_message(&[
        Some("DELETE_AVATAR"),
        Some(site_id_val.as_str()),
        Some(challenge),
    ]);
    if !verify_signature(&author_public_key, &message, &author_signature) {
        return Err(AppError::InvalidSignature);
    }

    state
        .driver
        .set_avatar_url(&author_public_key, &site_id_val, None)
        .await
        .map_err(|e| AppError::Internal(format!("failed to remove avatar: {e}")))?;
    Ok(Json(serde_json::json!({})))
}

/// Lists a site's projected sticker packs for visitors.
pub(crate) async fn list_stickers_handler(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(site_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = client_key(&headers, Some(addr), &state.trusted_proxies);
    if !state.public_read_limiter.allow(&key) {
        return Err(AppError::TooManyRequests {
            detail: "public reads are rate limited; try again later".to_string(),
            retry_after_seconds: state.public_read_limiter.window().as_secs(),
        });
    }
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    if state
        .store
        .get_site(&site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up site: {e}")))?
        .is_none()
    {
        return Err(AppError::NotFound("Site not found.".to_string()));
    }
    let media_base = media_url_base(&state, &headers, Some(addr));
    let packs = list_site_sticker_packs(state.store.as_ref(), site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to list sticker packs: {e}")))?;
    let packs = packs
        .iter()
        .map(|projection| {
            pack_response_shape(
                &projection.pack,
                |url| {
                    state
                        .media_proxy
                        .as_ref()
                        .and_then(|proxy| proxy.proxify(url, &media_base))
                },
                |url| {
                    state
                        .media_proxy
                        .as_ref()
                        .and_then(|proxy| proxy.proxify_avatar(url, &media_base))
                },
            )
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
    let media_base = media_url_base(&state, &headers, Some(connect.0));
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
    Ok(Json(pack_response_shape(
        &projection.pack,
        |url| {
            state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify(url, &media_base))
        },
        |url| {
            state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify_avatar(url, &media_base))
        },
    )))
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
    let media_base = media_url_base(&state, &headers, Some(connect.0));
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
    Ok(Json(pack_response_shape(
        &projection.pack,
        |url| {
            state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify(url, &media_base))
        },
        |url| {
            state
                .media_proxy
                .as_ref()
                .and_then(|proxy| proxy.proxify_avatar(url, &media_base))
        },
    )))
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

    // Validate the identity before doing rate-limit bookkeeping or upstream
    // I/O. The same rule is enforced again by verify/fetch as a boundary.
    if !is_valid_media_id(&media_id) {
        return Err(AppError::BadRequest("invalid media id".to_string()));
    }

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
    let thumb =
        match (query.get("width"), query.get("height")) {
            (None, None) => None,
            (Some(width), Some(height)) => {
                let width = width
                    .parse::<u32>()
                    .map_err(|_| AppError::BadRequest("width must be an integer".to_string()))?;
                let height = height
                    .parse::<u32>()
                    .map_err(|_| AppError::BadRequest("height must be an integer".to_string()))?;
                let method = match query.get("method").map(String::as_str) {
                    None => None,
                    Some("crop") => Some(ThumbMethod::Crop),
                    Some("scale") => Some(ThumbMethod::Scale),
                    Some(_) => {
                        return Err(AppError::BadRequest(
                            "method must be crop or scale".to_string(),
                        ));
                    }
                };
                Some(ThumbParams::new(width, height, method).ok_or_else(|| {
                    AppError::BadRequest("invalid thumbnail dimensions".to_string())
                })?)
            }
            _ => {
                return Err(AppError::BadRequest(
                    "width and height must be provided together".to_string(),
                ));
            }
        };
    let signature = query
        .get("sig")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("missing signature".to_string()))?;
    if !proxy.verify(&server, &media_id, thumb, expires, &signature) {
        return Err(AppError::BadRequest(
            "invalid or expired media URL".to_string(),
        ));
    }

    let upstream = proxy
        .fetch(&server, &media_id, thumb)
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
    let presentation = classify_media_content_type(&content_type, thumb.is_some())
        .ok_or_else(|| AppError::BadRequest("unsupported media type".to_string()))?;

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

    Ok(media_response(presentation, bytes))
}

/// Builds the public response from an already-classified upstream payload.
fn media_response(presentation: MediaPresentation, bytes: Vec<u8>) -> Response {
    let disposition = format!(
        "{}; filename=\"{}\"",
        presentation.disposition, presentation.filename
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, presentation.content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(MEDIA_CONTENT_SECURITY_POLICY),
        )
        .header("cross-origin-resource-policy", "cross-origin")
        .header(header::REFERRER_POLICY, "no-referrer")
        .body(Body::from(bytes))
        .expect("static response builds")
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
    use crate::trusted_proxy::TrustedProxyRule;
    use cumments_core::models::{
        AuthorKind, AuthorSnapshot, MediaContent, MediaKind, Message, MessageStatus, TextContent,
        TextStyle,
    };

    fn proxy() -> MediaProxy {
        MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            None,
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

    fn thumb(width: u32, height: u32, method: Option<ThumbMethod>) -> Option<ThumbParams> {
        Some(ThumbParams {
            width,
            height,
            method,
        })
    }

    #[test]
    fn proxify_rewrites_mxc_urls_and_verify_round_trips() {
        let p = proxy();
        let url = p.proxify("mxc://hs/abc", "").expect("proxify");
        assert!(url.starts_with("/api/v1/media/hs/abc?expires="));
        let params = query_params(&url);
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(p.verify("hs", "abc", None, expires, &params["sig"]));
    }

    #[test]
    fn proxify_serves_foreign_servers() {
        let p = proxy();
        let url = p.proxify("mxc://other/abc", "").expect("foreign proxify");
        assert!(url.starts_with("/api/v1/media/other/abc?expires="));
        let params = query_params(&url);
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(
            p.verify("other", "abc", None, expires, &params["sig"]),
            "foreign-server URLs must verify"
        );
    }

    #[test]
    fn proxify_avatar_and_thumbnail_request_their_default_variants() {
        let p = proxy();
        let avatar = p.proxify_avatar("mxc://hs/avatar", "").expect("avatar");
        let params = query_params(&avatar);
        assert_eq!(params.get("width").map(String::as_str), Some("96"));
        assert_eq!(params.get("height").map(String::as_str), Some("96"));
        assert_eq!(params.get("method").map(String::as_str), Some("crop"));
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(p.verify(
            "hs",
            "avatar",
            thumb(96, 96, Some(ThumbMethod::Crop)),
            expires,
            &params["sig"]
        ));

        let content = p
            .proxify_thumbnail("mxc://hs/thumb", "")
            .expect("thumbnail");
        let params = query_params(&content);
        assert_eq!(params.get("width").map(String::as_str), Some("320"));
        assert_eq!(params.get("height").map(String::as_str), Some("240"));
        assert_eq!(params.get("method").map(String::as_str), Some("scale"));
    }

    #[test]
    fn proxify_rejects_private_and_malformed_servers() {
        let p = proxy();
        assert!(p.proxify("mxc://127.0.0.1/abc", "").is_none());
        assert!(p.proxify("mxc://10.0.0.1/abc", "").is_none());
        assert!(p.proxify("mxc://::1/abc", "").is_none());
        assert!(p.proxify("mxc://bad server/abc", "").is_none());
        assert!(p.proxify("mxc://-bad/abc", "").is_none());
        assert!(p.proxify("mxc://bad-/abc", "").is_none());

        let open = MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            None,
            true,
        )
        .expect("build open proxy");
        assert!(open.proxify("mxc://127.0.0.1/abc", "").is_some());
    }

    #[test]
    fn proxify_prepends_public_base_url_when_configured() {
        let p = MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            Some("https://comments.example.net/".to_string()),
            false,
        )
        .expect("build proxy with public base");
        let base = p
            .public_base(&HeaderMap::new(), None, &TrustedProxySet::default())
            .expect("configured base wins");
        assert_eq!(base, "https://comments.example.net");
        let url = p.proxify("mxc://hs/abc", &base).expect("proxify");
        assert!(url.starts_with("https://comments.example.net/api/v1/media/hs/abc?expires="));
        let params = query_params(&url);
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(p.verify("hs", "abc", None, expires, &params["sig"]));

        let bad = MediaProxy::new(
            "http://hs".to_string(),
            "token".to_string(),
            "sign-key".to_string(),
            Some("comments.example.net".to_string()),
            false,
        );
        assert!(bad.is_err(), "public base without scheme must be rejected");
    }

    #[test]
    fn request_base_uses_host_header_by_default() {
        let p = proxy();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("comments.example.net"),
        );
        let base = p
            .public_base(&headers, None, &TrustedProxySet::default())
            .expect("host-derived base");
        assert_eq!(base, "http://comments.example.net");
    }

    #[test]
    fn request_base_prefers_forwarded_headers_from_trusted_peer() {
        let p = proxy();
        let trusted = TrustedProxySet::from_rules(&[TrustedProxyRule::parse("loopback").unwrap()])
            .expect("trusted rules");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("internal.example.net"),
        );
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("comments.example.net"),
        );
        let addr = Some("127.0.0.1:54321".parse().expect("addr"));
        let base = p
            .public_base(&headers, addr, &trusted)
            .expect("forwarded base");
        assert_eq!(base, "https://comments.example.net");
    }

    #[test]
    fn request_base_ignores_forwarded_headers_from_untrusted_peer() {
        let p = proxy();
        let trusted = TrustedProxySet::from_rules(&[TrustedProxyRule::parse("loopback").unwrap()])
            .expect("trusted rules");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("comments.example.net"),
        );
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("evil.example.net"),
        );
        // Untrusted peer: forwarded headers are ignored and http is assumed.
        let addr = Some("203.0.113.9:54321".parse().expect("addr"));
        let base = p
            .public_base(&headers, addr, &trusted)
            .expect("host-derived base");
        assert_eq!(base, "http://comments.example.net");
    }

    #[test]
    fn request_base_rejects_malformed_hosts() {
        let p = proxy();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("comments.example.net/path"),
        );
        assert!(
            p.public_base(&headers, None, &TrustedProxySet::default())
                .is_none()
        );
        headers.insert(
            header::HOST,
            HeaderValue::from_static("user@comments.example.net"),
        );
        assert!(
            p.public_base(&headers, None, &TrustedProxySet::default())
                .is_none()
        );
    }

    #[test]
    fn verify_rejects_tampered_and_expired_urls() {
        let p = proxy();
        let params = query_params(&p.proxify("mxc://hs/abc", "").expect("proxify"));
        let expires: i64 = params["expires"].parse().expect("expires");
        assert!(!p.verify("hs", "abc", None, expires, "deadbeef"));
        assert!(!p.verify("hs", "abc", None, expires - 1000, &params["sig"]));
        // The signature covers the server, so a URL minted for another
        // server cannot be replayed against it.
        assert!(!p.verify("other", "abc", None, expires, &params["sig"]));
        assert!(!p.verify("hs", "abc", None, expires, "not-a-signature"));
    }

    #[test]
    fn verify_rejects_thumbnail_parameter_tampering() {
        let p = proxy();
        let avatar = p.proxify_avatar("mxc://hs/avatar", "").expect("avatar");
        let params = query_params(&avatar);
        let expires: i64 = params["expires"].parse().expect("expires");
        // The signature binds the requested size: resizing the URL must fail
        // verification even though server/media/expiry are untouched.
        assert!(!p.verify(
            "hs",
            "avatar",
            thumb(320, 240, Some(ThumbMethod::Scale)),
            expires,
            &params["sig"]
        ));
        assert!(!p.verify(
            "hs",
            "avatar",
            thumb(96, 96, Some(ThumbMethod::Scale)),
            expires,
            &params["sig"]
        ));
    }

    #[test]
    fn thumbnail_dimensions_are_bounded() {
        assert!(ThumbParams::new(96, 96, Some(ThumbMethod::Crop)).is_some());
        assert!(ThumbParams::new(0, 0, None).is_none());
        assert!(ThumbParams::new(96, 0, None).is_none());
        assert!(ThumbParams::new(MAX_THUMBNAIL_DIMENSION + 1, 96, None).is_none());
    }

    #[test]
    fn upstream_media_path_uses_authenticated_v1_endpoints() {
        assert_eq!(
            upstream_media_path("hs", "abc", None),
            "_matrix/client/v1/media/download/hs/abc"
        );
        assert_eq!(
            upstream_media_path("hs", "abc", thumb(96, 96, Some(ThumbMethod::Crop))),
            "_matrix/client/v1/media/thumbnail/hs/abc?width=96&height=96&method=crop"
        );
        assert_eq!(
            upstream_media_path("hs", "abc", thumb(320, 240, None)),
            "_matrix/client/v1/media/thumbnail/hs/abc?width=320&height=240"
        );
    }

    #[test]
    fn proxify_message_rewrites_media_and_thumbnail_urls() {
        let p = proxy();
        let mut message = Message {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
            author: AuthorSnapshot {
                kind: AuthorKind::Matrix,
                display_name: None,
                avatar_url: Some("mxc://hs/avatar".to_string()),
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
        p.proxify_message(&mut message, "");
        match message.content {
            Content::Media(media) => {
                assert!(media.url.starts_with("/api/v1/media/hs/abc?"));
                let thumbnail = media.thumbnail_url.expect("thumbnail rewritten");
                assert!(
                    thumbnail.starts_with("/api/v1/media/hs/thumb?")
                        && thumbnail.contains("width=320")
                        && thumbnail.contains("height=240")
                        && thumbnail.contains("method=scale"),
                    "thumbnail URL must request the content thumbnail variant: {thumbnail}"
                );
            }
            other => panic!("expected media, got {other:?}"),
        }
        let avatar = message.author.avatar_url.expect("author avatar rewritten");
        assert!(
            avatar.starts_with("/api/v1/media/hs/avatar?")
                && avatar.contains("width=96")
                && avatar.contains("height=96")
                && avatar.contains("method=crop"),
            "author avatar must use the avatar thumbnail variant: {avatar}"
        );
    }

    #[test]
    fn text_content_is_not_rewritten() {
        let p = proxy();
        let mut message = Message {
            event_id: "$e:hs".to_string(),
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
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
        p.proxify_message(&mut message, "");
        assert!(matches!(message.content, Content::Text(_)));
    }

    #[test]
    fn media_id_validation_follows_the_matrix_whitelist() {
        assert!(is_valid_media_id("abc-_XYZ123"));
        assert!(is_valid_media_id(&"a".repeat(MAX_MEDIA_ID_BYTES)));

        assert!(!is_valid_media_id(""));
        assert!(!is_valid_media_id(&"a".repeat(MAX_MEDIA_ID_BYTES + 1)));
        assert!(!is_valid_media_id("a/b"));
        assert!(!is_valid_media_id("../secret"));
        assert!(!is_valid_media_id("%2Fsecret"));
        assert!(!is_valid_media_id("cat.png"));
        assert!(!is_valid_media_id("cat png"));
        assert!(!is_valid_media_id("cat\"png"));
        assert!(!is_valid_media_id("cat;png"));
        assert!(!is_valid_media_id("cat\npng"));
        assert!(!is_valid_media_id("cat🐱"));
    }

    #[test]
    fn proxify_rejects_unsafe_media_ids() {
        let p = proxy();
        for media_id in [
            "",
            "a/b",
            "../secret",
            "%2Fsecret",
            "cat.png",
            "cat png",
            "cat\"png",
            "cat;png",
            "cat\npng",
            "cat🐱",
        ] {
            let url = format!("mxc://hs/{media_id}");
            assert!(
                p.proxify(&url, "").is_none(),
                "unsafe media id must not be signed: {media_id}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_rejects_unsafe_media_ids_before_network_io() {
        let p = proxy();
        assert!(p.fetch("hs", "cat\"png", None).await.is_err());
        assert!(p.fetch("hs", "../../secret", None).await.is_err());
    }

    #[test]
    fn media_content_types_are_classified_and_canonicalized() {
        let png = classify_media_content_type("Image/PNG; charset=utf-8", false)
            .expect("PNG is inline-safe");
        assert_eq!(png.content_type, "image/png");
        assert_eq!(png.disposition, "inline");
        assert_eq!(png.filename, "media.png");

        let svg = classify_media_content_type("IMAGE/SVG+XML", false).expect("SVG can download");
        assert_eq!(svg.content_type, "image/svg+xml");
        assert_eq!(svg.disposition, "attachment");
        assert_eq!(svg.filename, "download.svg");

        assert!(
            classify_media_content_type("application/pdf", false)
                .is_some_and(|pdf| pdf.disposition == "attachment")
        );
        assert!(classify_media_content_type("text/html", false).is_none());
    }

    #[test]
    fn thumbnail_content_types_are_restricted_to_matrix_allowlist() {
        let webp = classify_media_content_type("image/webp", true).expect("thumbnail type");
        assert_eq!(webp.disposition, "inline");
        assert_eq!(webp.filename, "thumbnail.webp");

        assert!(classify_media_content_type("image/svg+xml", true).is_none());
        assert!(classify_media_content_type("application/pdf", true).is_none());
    }

    #[test]
    fn media_response_headers_are_fixed_and_safe() {
        let presentation =
            classify_media_content_type("IMAGE/PNG; bogus-parameter=1", false).expect("classify");
        let response = media_response(presentation, b"bytes".to_vec());
        let headers = response.headers();

        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            headers[header::CONTENT_DISPOSITION],
            "inline; filename=\"media.png\""
        );
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            MEDIA_CONTENT_SECURITY_POLICY
        );
        assert_eq!(headers["cross-origin-resource-policy"], "cross-origin");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        assert!(
            !headers[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains("media-id")
        );
    }
}
