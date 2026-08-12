//! hs_token verification for AppService push requests.

use axum::http::HeaderMap;
use cumments_core::site_auth::constant_time_eq;
use std::collections::HashMap;

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
}

/// Resolve the `hs_token` from the standard header and/or the legacy query
/// parameter. The header takes precedence; when both are supplied they must
/// match, otherwise no token is considered valid.
pub(super) fn received_hs_token<'a>(
    headers: &'a HeaderMap,
    query: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let header_token = bearer_token(headers);
    let query_token = query.get("hs_token").map(|s| s.as_str());
    match (header_token, query_token) {
        (Some(header), Some(query)) if header == query => Some(header),
        (Some(header), None) => Some(header),
        (None, Some(query)) => Some(query),
        _ => None,
    }
}

/// Whether the resolved `hs_token` matches the configured value.
pub(super) fn hs_token_matches(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    expected: &str,
) -> bool {
    match received_hs_token(headers, query) {
        Some(received) => constant_time_eq(received.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_token_accepts_standard_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc123"));
        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_scheme_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("bearer abc123"));
        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_rejects_missing_or_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer"));
        assert_eq!(bearer_token(&headers), None);

        headers.insert("authorization", HeaderValue::from_static("Basic abc123"));
        assert_eq!(bearer_token(&headers), None);

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_str(token).unwrap());
        headers
    }

    #[test]
    fn hs_token_accepts_header_only() {
        let query = HashMap::new();
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer abc123"), &query),
            Some("abc123")
        );
    }

    #[test]
    fn hs_token_accepts_legacy_query_only() {
        let headers = HeaderMap::new();
        let mut query = HashMap::new();
        query.insert("hs_token".to_string(), "abc123".to_string());
        assert_eq!(received_hs_token(&headers, &query), Some("abc123"));
    }

    #[test]
    fn hs_token_requires_agreement_when_both_are_present() {
        let mut query = HashMap::new();
        query.insert("hs_token".to_string(), "query123".to_string());

        // Both present and matching: accepted.
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer query123"), &query),
            Some("query123")
        );
        // Both present but disagreeing: no token is valid.
        assert_eq!(
            received_hs_token(&headers_with_bearer("Bearer header123"), &query),
            None
        );
    }

    #[test]
    fn hs_token_rejects_missing_tokens() {
        assert_eq!(received_hs_token(&HeaderMap::new(), &HashMap::new()), None);
    }
}
