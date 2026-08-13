//! Write-path site authentication and derived CORS behavior.
//!
//! Replaces the old global `CorsLayer`: CORS headers are derived from the
//! site registry instead of a `cors_origins` config value. Reads stay public
//! (`Access-Control-Allow-Origin: *`); writes are gated by the instance
//! policy and the site's auth mode (`origin` or `secret`).

use crate::ApiState;
use crate::error::AppError;
use crate::routes::comments::QUERY_METHOD;
use crate::routes::media::MEDIA_MAX_BYTES;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_REQUEST_METHOD, ORIGIN, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use cumments_core::models::ID_REGEX;
use cumments_core::site_auth::{
    Origin, OriginPattern, SITE_SIGNATURE_HEADER, SITE_SIGNATURE_MAX_SKEW_SECONDS,
    SITE_TIMESTAMP_HEADER, SiteAuthInfo, SiteAuthMode, SitePolicyEntry, SiteVerificationPolicy,
    is_timestamp_fresh, verify_site_request_signature,
};

const BODY_LIMIT: usize = 1024 * 1024;

/// Middleware for the comment write/read routes.
pub async fn enforce_site_auth(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();

    if method == Method::OPTIONS {
        return handle_preflight(&state, req.uri().path().to_string(), req.headers().clone()).await;
    }

    if !is_write_method(&method) {
        return public_read_response(next.run(req).await);
    }

    let Some(site_id) = site_id_from_path(req.uri().path()) else {
        return next.run(req).await;
    };
    if !ID_REGEX.is_match(&site_id) {
        let response = AppError::BadRequest(format!("invalid site id `{site_id}`")).into_response();
        return maybe_disabled_wildcard(response, &state);
    }
    let path = req.uri().path().to_string();

    // Only secret-mode authorization needs the raw body (the HMAC covers
    // `sha256(body)`). Origin/disabled modes decide from headers alone, so
    // those requests pass through untouched and never hit the generic 1MB
    // cap — media uploads (up to `MEDIA_MAX_BYTES`) keep working.
    let needs_body = match site_uses_secret_auth(&state, &site_id).await {
        Ok(secret_auth) => {
            state.site_auth_policy.verification != SiteVerificationPolicy::Disabled && secret_auth
        }
        Err(error) => return error.into_response(),
    };

    let mut bytes: Option<Vec<u8>> = None;
    let req = if needs_body {
        let limit = if is_media_upload_path(&path) {
            MEDIA_MAX_BYTES
        } else {
            BODY_LIMIT
        };
        let (parts, body) = req.into_parts();
        let body_bytes = match to_bytes(body, limit).await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => {
                let response =
                    AppError::BadRequest("request body is too large".to_string()).into_response();
                return maybe_disabled_wildcard(response, &state);
            }
        };
        bytes = Some(body_bytes.clone());
        Request::from_parts(parts, Body::from(body_bytes))
    } else {
        req
    };

    match authorize_site_write(
        &state,
        &site_id,
        &method,
        &path,
        req.headers(),
        bytes.as_deref().unwrap_or(&[]),
    )
    .await
    {
        Ok(Some(allowed_origin)) => {
            let mut response = next.run(req).await;
            add_allow_origin(&mut response, &allowed_origin);
            response
        }
        Ok(None) => {
            let mut response = next.run(req).await;
            if state.site_auth_policy.verification == SiteVerificationPolicy::Disabled {
                // `disabled` is intentionally permissive: opaque origins such
                // as `file://` pages (serialized as `Origin: null`) are
                // allowed, and the browser still needs a CORS header to read
                // the response. Use `*` because `null` is not an http(s)
                // origin we can echo, and the API does not rely on cookies.
                add_wildcard_origin(&mut response);
            }
            response
        }
        Err(error) => maybe_disabled_wildcard(error.into_response(), &state),
    }
}

/// Whether the effective auth mode for a site is HMAC-secret based.
///
/// Mirrors the mode-resolution order in [`authorize_site_write`]: the
/// operator-declared config overlay wins over the database value, and both
/// missing means `origin`. This is a separate lookup so the caller can decide
/// whether the request body must be buffered before authorization runs.
async fn site_uses_secret_auth(state: &ApiState, site_id: &str) -> Result<bool, AppError> {
    let config_entry = state.site_auth_policy.entry(site_id);
    let db_auth = state
        .store
        .get_site_auth(site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load site auth: {e}")))?;
    let mode = config_entry
        .and_then(|entry| entry.auth_mode)
        .or_else(|| db_auth.as_ref().map(|info| info.auth_mode))
        .unwrap_or(SiteAuthMode::Origin);
    Ok(mode == SiteAuthMode::Secret)
}

/// In `disabled` mode every write response is readable cross-origin, even
/// early validation failures, so browsers surface the JSON error instead of
/// a generic `NetworkError`.
fn maybe_disabled_wildcard(response: Response, state: &ApiState) -> Response {
    if state.site_auth_policy.verification != SiteVerificationPolicy::Disabled {
        return response;
    }
    let mut response = response;
    add_wildcard_origin(&mut response);
    response
}

/// Middleware for public routes: reads, registration and verification.
pub async fn public_cors(req: Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        return preflight_response(None);
    }
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

fn is_write_method(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PATCH | Method::DELETE)
}

/// `/api/v1/sites/{site_id}/posts/...` → the site id segment.
pub(crate) fn site_id_from_path(path: &str) -> Option<String> {
    let segments = path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    segments.get(3).map(|s| s.to_string())
}

/// Whether a path is the guest media upload route
/// (`/api/v1/sites/{site}/posts/{post}/media`). Kept in sync with the route
/// table in `build_router`; used only to choose the body-buffering limit for
/// HMAC verification.
fn is_media_upload_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() == 7
        && segments[0] == "api"
        && segments[1] == "v1"
        && segments[2] == "sites"
        && segments[4] == "posts"
        && segments[6] == "media"
}

/// Extracts the single `Origin` header, if any.
///
/// Rejects multiple headers. The opaque `Origin: null` value is rejected
/// unless the caller explicitly opts in via `allow_null` (see
/// CVE-2026-27978: null must be treated as an explicit origin value, never as
/// a missing header). The dev-only `disabled` policy opts in so `file://`
/// demo pages keep working; `optional` and `required` do not.
fn request_origin(headers: &HeaderMap, allow_null: bool) -> Result<Option<Origin>, AppError> {
    let values = headers
        .get_all(ORIGIN)
        .iter()
        .collect::<Vec<&HeaderValue>>();
    match values.as_slice() {
        [] => Ok(None),
        [value] => {
            let value = value
                .to_str()
                .map_err(|_| AppError::SiteOriginDenied("invalid Origin header".to_string()))?;
            if value == "null" {
                return if allow_null {
                    Ok(None)
                } else {
                    Err(AppError::SiteOriginDenied(
                        "`Origin: null` is not allowed".to_string(),
                    ))
                };
            }
            Origin::parse(value)
                .map(Some)
                .map_err(|e| AppError::SiteOriginDenied(format!("invalid Origin: {e}")))
        }
        _ => Err(AppError::SiteOriginDenied(
            "multiple Origin headers are not allowed".to_string(),
        )),
    }
}

/// The core write authorization decision.
///
/// Returns `Ok(Some(origin))` when the request is allowed in origin mode and
/// the response should echo that origin; `Ok(None)` when allowed without a
/// CORS echo (secret mode, or `disabled` with a missing/opaque Origin header).
pub async fn authorize_site_write(
    state: &ApiState,
    site_id: &str,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Option<Origin>, AppError> {
    let policy = &state.site_auth_policy;
    let allow_null = policy.verification == SiteVerificationPolicy::Disabled;
    let origin = request_origin(headers, allow_null)?;

    let config_entry = policy.entry(site_id);
    let db_auth = state
        .store
        .get_site_auth(site_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load site auth: {e}")))?;

    // A site must exist before it can be written to, in every verification
    // policy. This is what keeps an unknown `site_id` from provisioning a
    // Matrix Space on its first comment: only registered (API/CLI) or
    // operator-declared (`[sites]`) sites reach the intent queue.
    if config_entry.is_none() && db_auth.is_none() {
        return Err(AppError::SiteNotRegistered(format!(
            "site `{site_id}` is not registered; create it with POST /api/v1/sites \
             or `cumments sites register`"
        )));
    }

    if policy.verification == SiteVerificationPolicy::Disabled {
        return Ok(origin);
    }

    let auth_mode = config_entry
        .and_then(|entry| entry.auth_mode)
        .or_else(|| db_auth.as_ref().map(|info| info.auth_mode))
        .unwrap_or(SiteAuthMode::Origin);

    match auth_mode {
        SiteAuthMode::Secret => {
            let secret = config_entry
                .and_then(|entry| entry.secret.clone())
                .or_else(|| db_auth.as_ref().and_then(|info| info.secret.clone()));
            let Some(secret) = secret else {
                return Err(AppError::SiteSignatureInvalid(format!(
                    "site `{site_id}` has no HMAC secret configured"
                )));
            };
            verify_hmac(&secret, method, path, headers, body).map(|_| None)
        }
        SiteAuthMode::Origin => decide_origin_write_access(
            policy.verification,
            config_entry,
            db_auth.as_ref(),
            site_id,
            origin,
        ),
    }
}

/// The pure origin-mode decision, separated from I/O so it can be unit-tested.
fn decide_origin_write_access(
    policy: SiteVerificationPolicy,
    config_entry: Option<&SitePolicyEntry>,
    db_auth: Option<&SiteAuthInfo>,
    site_id: &str,
    origin: Option<Origin>,
) -> Result<Option<Origin>, AppError> {
    let mut allowed = config_entry
        .map(|entry| entry.allowed_origins.clone())
        .unwrap_or_default();
    if let Some(info) = db_auth {
        allowed.extend(
            info.verified_origins
                .iter()
                .cloned()
                .map(OriginPattern::Exact),
        );
    }

    if allowed.is_empty() {
        return match policy {
            // A caller-chosen id is a privilege: even in `optional` mode the
            // owner must prove a domain before writes are accepted. Random
            // server-generated ids keep the relaxed migration behavior.
            SiteVerificationPolicy::Optional if db_auth.is_some_and(|info| info.is_custom_id) => {
                Err(AppError::SiteVerificationRequired(format!(
                    "site `{site_id}` uses a caller-chosen id; verify its origin or add it \
                     to the `[sites]` configuration before writing comments"
                )))
            }
            SiteVerificationPolicy::Optional => {
                tracing::warn!(
                    site_id,
                    "accepting a write for an unverified site in optional mode"
                );
                Ok(origin)
            }
            SiteVerificationPolicy::Required => Err(AppError::SiteVerificationRequired(format!(
                "site `{site_id}` is not verified; verify its origin or add it to \
                     the `[sites]` configuration before writing comments"
            ))),
            SiteVerificationPolicy::Disabled => unreachable!(),
        };
    }

    let Some(origin) = origin else {
        return Err(AppError::SiteOriginDenied(format!(
            "site `{site_id}` requires an Origin header on write requests"
        )));
    };
    if allowed.iter().any(|pattern| pattern.matches(&origin)) {
        Ok(Some(origin))
    } else {
        Err(AppError::SiteOriginDenied(format!(
            "origin `{}` is not allowed for site `{site_id}`",
            origin.as_str()
        )))
    }
}

fn verify_hmac(
    secret: &str,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), AppError> {
    let timestamp = header_value(headers, SITE_TIMESTAMP_HEADER, "timestamp")?;
    if !is_timestamp_fresh(timestamp, Utc::now(), SITE_SIGNATURE_MAX_SKEW_SECONDS) {
        return Err(AppError::SiteSignatureInvalid(
            "request timestamp is missing or outside the accepted window".to_string(),
        ));
    }
    let signature = header_value(headers, SITE_SIGNATURE_HEADER, "signature")?;
    if !verify_site_request_signature(
        secret.as_bytes(),
        timestamp,
        method.as_str(),
        path,
        body,
        signature,
    ) {
        return Err(AppError::SiteSignatureInvalid(
            "HMAC signature does not match the request".to_string(),
        ));
    }
    Ok(())
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str, label: &str) -> Result<&'a str, AppError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::SiteSignatureInvalid(format!("missing {label} header")))
}

async fn handle_preflight(state: &ApiState, path: String, headers: HeaderMap) -> Response {
    let Some(requested_method) = headers
        .get(ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Method::from_bytes(value.as_bytes()).ok())
    else {
        return AppError::BadRequest("missing Access-Control-Request-Method".to_string())
            .into_response();
    };

    if requested_method == Method::GET || requested_method == *QUERY_METHOD {
        return preflight_response(None);
    }

    if state.site_auth_policy.verification == SiteVerificationPolicy::Disabled {
        return preflight_response(None);
    }

    let Some(site_id) = site_id_from_path(&path) else {
        return preflight_response(None);
    };

    let decision =
        authorize_site_write(state, &site_id, &requested_method, &path, &headers, &[]).await;
    match decision {
        Ok(Some(allowed)) => preflight_response(Some(allowed)),
        Ok(None) => AppError::SiteOriginDenied(
            "secret-mode sites do not accept browser preflights; call through the site backend"
                .to_string(),
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}

fn preflight_response(allowed_origin: Option<Origin>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    match allowed_origin {
        Some(origin) => {
            headers.insert(
                ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(origin.as_str()).expect("origin is a valid header value"),
            );
            headers.insert(VARY, HeaderValue::from_static("origin"));
        }
        None => {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        }
    }
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, QUERY, POST, PATCH, DELETE, OPTIONS"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, idempotency-key"),
    );
    response
}

fn public_read_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

fn add_allow_origin(response: &mut Response, origin: &Origin) {
    response.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(origin.as_str()).expect("origin is a valid header value"),
    );
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("origin"));
}

fn add_wildcard_origin(response: &mut Response) {
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use cumments_core::site_auth::{
        SiteAuthInfo, SiteAuthMode, SitePolicyEntry, SiteVerificationStatus,
    };

    fn origin(value: &str) -> Origin {
        Origin::parse(value).expect("valid test origin")
    }

    fn config_entry(origins: &[&str]) -> Option<SitePolicyEntry> {
        Some(SitePolicyEntry {
            auth_mode: None,
            allowed_origins: origins
                .iter()
                .map(|raw| OriginPattern::parse(raw).expect("valid pattern"))
                .collect(),
            secret: None,
        })
    }

    fn db_auth(origins: &[&str]) -> Option<SiteAuthInfo> {
        Some(SiteAuthInfo {
            site_id: "test-site".to_string(),
            is_custom_id: false,
            auth_mode: SiteAuthMode::Origin,
            verification_status: SiteVerificationStatus::Verified,
            verified_origins: origins.iter().map(|raw| origin(raw)).collect(),
            verified_at: None,
            secret: None,
            claim_token_hash: None,
            updated_at: None,
        })
    }

    #[test]
    fn optional_policy_allows_unverified_sites() {
        let result = decide_origin_write_access(
            SiteVerificationPolicy::Optional,
            None,
            None,
            "test-site",
            Some(origin("https://anywhere.example.com")),
        );
        assert!(matches!(result, Ok(Some(_))));
    }

    #[test]
    fn optional_policy_rejects_unverified_custom_named_sites() {
        let mut info = db_auth(&[]).expect("db auth");
        info.is_custom_id = true;
        let result = decide_origin_write_access(
            SiteVerificationPolicy::Optional,
            None,
            Some(&info),
            "test-site",
            Some(origin("https://anywhere.example.com")),
        );
        assert!(matches!(result, Err(AppError::SiteVerificationRequired(_))));
    }

    #[test]
    fn required_policy_rejects_unverified_sites() {
        let result = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            None,
            None,
            "test-site",
            Some(origin("https://anywhere.example.com")),
        );
        assert!(matches!(result, Err(AppError::SiteVerificationRequired(_))));
    }

    #[test]
    fn exact_and_wildcard_origins_are_matched() {
        let allowed = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            config_entry(&["https://blog.example.com", "https://*.example.net"]).as_ref(),
            None,
            "test-site",
            Some(origin("https://blog.example.com")),
        );
        assert!(matches!(allowed, Ok(Some(_))));

        let wildcard = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            config_entry(&["https://*.example.net"]).as_ref(),
            None,
            "test-site",
            Some(origin("https://docs.example.net")),
        );
        assert!(matches!(wildcard, Ok(Some(_))));

        let denied = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            config_entry(&["https://blog.example.com"]).as_ref(),
            None,
            "test-site",
            Some(origin("https://evil.example.net")),
        );
        assert!(matches!(denied, Err(AppError::SiteOriginDenied(_))));
    }

    #[test]
    fn db_verified_origins_are_merged_with_config() {
        let result = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            config_entry(&["https://blog.example.com"]).as_ref(),
            db_auth(&["https://notes.example.com"]).as_ref(),
            "test-site",
            Some(origin("https://notes.example.com")),
        );
        assert!(matches!(result, Ok(Some(_))));
    }

    #[test]
    fn verified_site_requires_an_origin_header() {
        let result = decide_origin_write_access(
            SiteVerificationPolicy::Required,
            config_entry(&["https://blog.example.com"]).as_ref(),
            None,
            "test-site",
            None,
        );
        assert!(matches!(result, Err(AppError::SiteOriginDenied(_))));
    }

    #[test]
    fn hmac_verification_checks_signature_and_timestamp() {
        let secret = "a-very-long-site-secret";
        let body = br#"{"content":"hi"}"#;
        let timestamp = Utc::now().timestamp().to_string();
        let signature = cumments_core::site_auth::site_request_signature(
            secret.as_bytes(),
            &timestamp,
            "POST",
            "/api/v1/sites/test-site/posts/hello/comments",
            body,
        );

        let mut headers = HeaderMap::new();
        headers.insert(SITE_TIMESTAMP_HEADER, timestamp.parse().unwrap());
        headers.insert(SITE_SIGNATURE_HEADER, signature.parse().unwrap());
        assert!(
            verify_hmac(
                secret,
                &Method::POST,
                "/api/v1/sites/test-site/posts/hello/comments",
                &headers,
                body
            )
            .is_ok()
        );

        let mut stale = headers.clone();
        stale.insert(
            SITE_TIMESTAMP_HEADER,
            (Utc::now().timestamp() - 600).to_string().parse().unwrap(),
        );
        assert!(
            verify_hmac(
                secret,
                &Method::POST,
                "/api/v1/sites/test-site/posts/hello/comments",
                &stale,
                body
            )
            .is_err()
        );

        let wrong_body = headers;
        assert!(
            verify_hmac(
                secret,
                &Method::POST,
                "/api/v1/sites/test-site/posts/hello/comments",
                &wrong_body,
                br#"{"content":"tampered"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn media_upload_path_detection_matches_the_upload_route() {
        assert!(is_media_upload_path(
            "/api/v1/sites/my-blog/posts/hello/media"
        ));
        assert!(!is_media_upload_path(
            "/api/v1/sites/my-blog/posts/hello/comments"
        ));
        assert!(!is_media_upload_path(
            "/api/v1/sites/my-blog/posts/hello/media/extra"
        ));
        assert!(!is_media_upload_path("/api/v1/sites/my-blog/media"));
        assert!(!is_media_upload_path("/health"));
    }
}
