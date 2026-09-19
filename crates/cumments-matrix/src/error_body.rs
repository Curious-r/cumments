//! Classification of Matrix Client-Server API error responses.
//!
//! Cumments sends its Matrix writes through the Client-Server API and reacts
//! to a few of the homeserver's structured error codes. Classification is
//! driven by the body's `errcode`, never by the HTTP status alone: a `413` (or
//! any other status) without `M_TOO_LARGE` is not proof that the homeserver
//! classified the request as too large.

use cumments_core::matrix_error::MatrixError;

/// The structured fields of a Matrix error response body.
pub(crate) struct MatrixErrorBody {
    errcode: String,
    error: Option<String>,
}

impl MatrixErrorBody {
    /// Parse a Matrix error response body.
    ///
    /// Returns `None` when the body is not Matrix error JSON carrying an
    /// `errcode`, so callers keep whatever generic handling they already have.
    pub(crate) fn parse(body: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let errcode = value.get("errcode")?.as_str()?.to_owned();
        Some(Self {
            errcode,
            error: value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_owned),
        })
    }

    /// The typed error Cumments gives this code, if it has special handling.
    pub(crate) fn classify(&self) -> Option<MatrixError> {
        match self.errcode.as_str() {
            "M_TOO_LARGE" => Some(MatrixError::RequestTooLarge {
                context: self.diagnostic(),
            }),
            _ => None,
        }
    }

    /// A diagnostic string that preserves the homeserver's own message.
    fn diagnostic(&self) -> String {
        match self.error.as_deref() {
            Some(error) if !error.is_empty() => format!("{}: {error}", self.errcode),
            _ => self.errcode.clone(),
        }
    }
}

/// The typed error for a Matrix error body, when Cumments classifies its code.
///
/// `None` means the code has no special handling here, so the caller keeps its
/// existing generic error.
pub(crate) fn typed_matrix_error(body: &str) -> Option<anyhow::Error> {
    let error = MatrixErrorBody::parse(body)?.classify()?;
    Some(anyhow::Error::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_m_too_large_by_its_errcode() {
        let error = typed_matrix_error(r#"{"errcode":"M_TOO_LARGE","error":"event too big"}"#)
            .expect("classified")
            .downcast::<MatrixError>()
            .expect("typed error");
        assert!(matches!(
            error,
            MatrixError::RequestTooLarge { context } if context.contains("event too big")
        ));
    }

    #[test]
    fn to_large_without_a_message_is_still_classified() {
        let error = typed_matrix_error(r#"{"errcode":"M_TOO_LARGE"}"#)
            .expect("classified")
            .downcast::<MatrixError>()
            .expect("typed error");
        assert!(matches!(
            error,
            MatrixError::RequestTooLarge { context } if context == "M_TOO_LARGE"
        ));
    }

    #[test]
    fn other_codes_and_malformed_bodies_are_not_classified() {
        // No special handling for unrelated codes.
        assert!(typed_matrix_error(r#"{"errcode":"M_FORBIDDEN","error":"nope"}"#).is_none());
        assert!(typed_matrix_error(r#"{"errcode":"M_NOT_FOUND","error":"gone"}"#).is_none());
        // Not Matrix error JSON at all.
        assert!(typed_matrix_error("<html>413 payload too large</html>").is_none());
        assert!(typed_matrix_error(r#"{"error":"too large"}"#).is_none());
        assert!(typed_matrix_error("").is_none());
        // The code must be structured, not merely mentioned in the text.
        assert!(typed_matrix_error(r#"{"error":"M_TOO_LARGE"}"#).is_none());
    }

    #[test]
    fn an_http_status_alone_never_classifies() {
        // A `413` response body without `M_TOO_LARGE` carries no Matrix verdict.
        let body = r#"{"errcode":"M_UNKNOWN","error":"payload too large"}"#;
        assert!(typed_matrix_error(body).is_none());
    }
}
