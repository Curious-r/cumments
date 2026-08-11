//! Site registration, verification and secret issuance route handlers.

use crate::ApiState;
use crate::error::AppError;
use crate::rate_limit::client_key;
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use cumments_core::models::SiteId;
use cumments_core::site_auth::{
    CLAIM_TOKEN_HEADER, Origin, SiteVerificationStatus, VerificationChallenge, VerificationMethod,
    VerificationToken, dns_proofs_match, issue_site_secret, parse_well_known_proofs, register_site,
    start_site_verification, token_hash, well_known_proofs_match,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use validator::Validate;

/// How many times each proof location is probed before giving up.
const PROOF_ATTEMPTS: usize = 3;
/// Delay between proof attempts, allowing transient failures to clear.
const PROOF_RETRY_DELAY: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Request / response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct StartVerificationRequest {
    #[validate(length(min = 1))]
    pub origins: Vec<String>,
    #[validate(length(min = 1))]
    pub methods: Vec<VerificationMethod>,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmVerificationRequest {
    pub origin: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct IssueSecretRequest {
    #[serde(default)]
    pub rotate: bool,
}

#[derive(Debug, Serialize)]
pub struct RegisterSiteResponse {
    pub site_id: String,
    pub claim_token: String,
}

#[derive(Debug, Serialize)]
pub struct VerificationChallengeResponse {
    pub site_id: String,
    pub token: String,
    pub methods: Vec<VerificationMethod>,
    pub origins: Vec<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub instructions: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ConfirmVerificationResponse {
    pub site_id: String,
    pub origin: String,
    pub status: &'static str,
    pub verified_origins: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IssueSecretResponse {
    pub site_id: String,
    pub secret: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn register_site_handler(
    State(state): State<ApiState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0));
    if !state.registration_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "site registration is rate limited; try again later".to_string(),
        ));
    }
    let registered = register_site(&*state.store)
        .await
        .map_err(|e| AppError::Internal(format!("failed to register site: {e}")))?;
    Ok((
        StatusCode::CREATED,
        Json(RegisterSiteResponse {
            site_id: registered.site_id,
            claim_token: registered.claim_token,
        }),
    ))
}

pub(crate) async fn start_verification_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Json(req): Json<StartVerificationRequest>,
) -> Result<impl IntoResponse, AppError> {
    let key = client_key(&headers, Some(connect.0));
    if !state.verification_limiter.allow(&key) {
        return Err(AppError::TooManyRequests(
            "verification issuance is rate limited; try again later".to_string(),
        ));
    }
    req.validate().map_err(AppError::Validation)?;
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let origins = req
        .origins
        .iter()
        .map(|raw| {
            Origin::parse(raw)
                .map_err(|e| AppError::BadRequest(format!("invalid origin `{raw}`: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let claim_token = claim_token_from_headers(&headers)?;

    let challenge = start_site_verification(
        &*state.store,
        &site_id,
        &origins,
        &req.methods,
        &claim_token,
    )
    .await
    .map_err(map_site_service_error)?;

    Ok(Json(VerificationChallengeResponse {
        site_id: challenge.site_id.clone(),
        token: challenge.token.clone(),
        methods: challenge.methods.clone(),
        origins: challenge
            .origins
            .iter()
            .map(|origin| origin.as_str().to_string())
            .collect(),
        expires_at: challenge.expires_at,
        instructions: build_instructions(&challenge),
    }))
}

pub(crate) async fn confirm_verification_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    Json(req): Json<ConfirmVerificationRequest>,
) -> Result<impl IntoResponse, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let origin = Origin::parse(&req.origin)
        .map_err(|e| AppError::BadRequest(format!("invalid origin `{}`: {e}", req.origin)))?;
    let token_hash = token_hash(&req.token);

    let token = state
        .store
        .find_verification_token(site_id.as_str(), &origin, &token_hash)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load verification token: {e}")))?
        .ok_or_else(|| {
            AppError::BadRequest(
                "no active verification found for this origin and token; start a new \
                 verification"
                    .to_string(),
            )
        })?;

    let proof_verified = verify_origin_proof(&site_id, &origin, &req.token, &token).await?;
    if !proof_verified {
        return Err(AppError::BadRequest(
            "proof not found in any requested location; check the published token and retry, \
             or start a new verification"
                .to_string(),
        ));
    }

    state
        .store
        .consume_verification_token(token.id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to consume verification token: {e}")))?;
    state
        .store
        .add_verified_origin(site_id.as_str(), &origin)
        .await
        .map_err(|e| AppError::Internal(format!("failed to record verified origin: {e}")))?;

    let auth = state
        .store
        .get_site_auth(site_id.as_str())
        .await
        .map_err(|e| AppError::Internal(format!("failed to reload site auth: {e}")))?
        .ok_or_else(|| AppError::NotFound("site not found after verification".to_string()))?;

    Ok(Json(ConfirmVerificationResponse {
        site_id: site_id.as_str().to_string(),
        origin: origin.as_str().to_string(),
        status: SiteVerificationStatus::Verified.as_str(),
        verified_origins: auth
            .verified_origins
            .iter()
            .map(|o| o.as_str().to_string())
            .collect(),
    }))
}

pub(crate) async fn issue_secret_handler(
    State(state): State<ApiState>,
    Path(site_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<IssueSecretRequest>,
) -> Result<impl IntoResponse, AppError> {
    let site_id = SiteId::new(site_id).map_err(AppError::Validation)?;
    let claim_token = claim_token_from_headers(&headers)?;
    let issued = issue_site_secret(&*state.store, &site_id, &claim_token, req.rotate)
        .await
        .map_err(map_site_service_error)?;

    Ok(Json(IssueSecretResponse {
        site_id: issued.site_id,
        secret: issued.secret,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn claim_token_from_headers(headers: &HeaderMap) -> Result<String, AppError> {
    headers
        .get(CLAIM_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            AppError::Unauthorized(format!(
                "missing {CLAIM_TOKEN_HEADER} header; the claim token was returned once when \
                 the site was registered"
            ))
        })
}

fn map_site_service_error(error: anyhow::Error) -> AppError {
    let message = error.to_string();
    if message.contains("claim token") {
        AppError::Unauthorized(message)
    } else if message.contains("already has a secret") {
        AppError::Conflict(message)
    } else {
        AppError::BadRequest(message)
    }
}

fn build_instructions(challenge: &VerificationChallenge) -> serde_json::Value {
    let mut instructions = serde_json::Map::new();
    for method in &challenge.methods {
        match method {
            VerificationMethod::WellKnown => {
                instructions.insert(
                    "well-known".to_string(),
                    serde_json::json!({
                        "description": format!(
                            "Publish the document below at {}/.well-known/cumments.json \
                             (the JSON file is static, so any SSG build can include it)",
                            challenge.origins[0].as_str()
                        ),
                        "document": {
                            "site_id": challenge.site_id,
                            "token": challenge.token,
                        },
                    }),
                );
            }
            VerificationMethod::Dns => {
                let host = challenge
                    .origins
                    .first()
                    .and_then(|origin| origin.host())
                    .unwrap_or_default();
                instructions.insert(
                    "dns".to_string(),
                    serde_json::json!({
                        "description": format!(
                            "Add a TXT record for `_cumments.{host}` with this value"
                        ),
                        "value": format!("site_id={},token={}", challenge.site_id, challenge.token),
                    }),
                );
            }
        }
    }
    serde_json::Value::Object(instructions)
}

/// Tries every requested proof location in order; the first match wins.
async fn verify_origin_proof(
    site_id: &SiteId,
    origin: &Origin,
    token: &str,
    stored: &VerificationToken,
) -> Result<bool, AppError> {
    for method in &stored.methods {
        for attempt in 0..PROOF_ATTEMPTS {
            let matched = match method {
                VerificationMethod::WellKnown => {
                    fetch_well_known_proof(origin, site_id.as_str(), token).await?
                }
                VerificationMethod::Dns => query_dns_proof(origin, site_id.as_str(), token).await?,
            };
            if matched {
                return Ok(true);
            }
            tracing::info!(
                site_id = site_id.as_str(),
                origin = origin.as_str(),
                method = method.as_str(),
                attempt = attempt + 1,
                "verification proof not found yet"
            );
            if attempt + 1 < PROOF_ATTEMPTS {
                tokio::time::sleep(PROOF_RETRY_DELAY).await;
            }
        }
    }
    Ok(false)
}

async fn fetch_well_known_proof(
    origin: &Origin,
    site_id: &str,
    token: &str,
) -> Result<bool, AppError> {
    let url = format!("{}/.well-known/cumments.json", origin.as_str());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build HTTP client: {e}")))?;

    let response = client.get(&url).send().await;
    let Ok(response) = response else {
        return Ok(false);
    };
    if !response.status().is_success() {
        return Ok(false);
    }
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read well-known document: {e}")))?;
    let proofs = parse_well_known_proofs(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid well-known document: {e}")))?;
    Ok(well_known_proofs_match(&proofs, site_id, token))
}

async fn query_dns_proof(origin: &Origin, site_id: &str, token: &str) -> Result<bool, AppError> {
    let Some(host) = origin.host() else {
        return Err(AppError::BadRequest(format!(
            "origin `{}` has no host for DNS verification",
            origin.as_str()
        )));
    };
    let name = format!("_cumments.{host}");
    let resolver = hickory_resolver::TokioAsyncResolver::tokio_from_system_conf()
        .map_err(|e| AppError::Internal(format!("failed to initialize DNS resolver: {e}")))?;
    let lookup = resolver
        .txt_lookup(name)
        .await
        .map_err(|e| AppError::Internal(format!("DNS TXT lookup failed: {e}")))?;
    let records = lookup
        .iter()
        .flat_map(|txt| {
            txt.txt_data()
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        })
        .collect::<Vec<_>>();
    Ok(dns_proofs_match(&records, site_id, token))
}
