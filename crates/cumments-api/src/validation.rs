//! Validation helpers for user-facing text measured in Unicode extended grapheme clusters.
//!
//! Length limits for user-facing text are measured in Unicode extended grapheme
//! clusters according to UAX #29, not UTF-8 bytes, Unicode scalar values, or
//! UTF-16 code units. This module centralizes grapheme counting so route
//! handlers and request DTOs share consistent semantics.

use unicode_segmentation::UnicodeSegmentation;
use validator::ValidationError;

/// Count Unicode extended grapheme clusters in `value`.
///
/// Uses `unicode_segmentation::UnicodeSegmentation::graphemes(true)` which
/// implements UAX #29 extended grapheme cluster boundaries. The input is not
/// normalized; segmentation is performed on the original bytes.
pub fn grapheme_len(value: &str) -> usize {
    value.graphemes(true).count()
}

fn validation_error(code: &'static str, message: String) -> ValidationError {
    let mut err = ValidationError::new(code);
    err.message = Some(message.into());
    err
}

/// Validate that `value` contains between `min` and `max` grapheme clusters (inclusive).
pub fn validate_grapheme_length(
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    let len = grapheme_len(value);
    if len < min || len > max {
        let mut err = validation_error(
            "grapheme_length",
            format!(
                "length must be between {} and {} grapheme clusters (got {})",
                min, max, len
            ),
        );
        err.add_param("min".into(), &min);
        err.add_param("max".into(), &max);
        err.add_param("value_len".into(), &len);
        return Err(err);
    }
    Ok(())
}

/// Validate that `value` contains at least `min` grapheme clusters.
///
/// Used for fields the application requires to be non-empty but deliberately
/// does not bound: an arbitrary field maximum is not a requirement of the
/// Matrix protocol, and where the protocol does bound something (the complete
/// event, the Poll answer count) the homeserver or the semantic layer decides.
pub fn validate_grapheme_min(value: &str, min: usize) -> Result<(), ValidationError> {
    let len = grapheme_len(value);
    if len < min {
        let mut err = validation_error(
            "grapheme_length",
            format!(
                "length must be at least {} grapheme clusters (got {})",
                min, len
            ),
        );
        err.add_param("min".into(), &min);
        err.add_param("value_len".into(), &len);
        return Err(err);
    }
    Ok(())
}

/// Validate that `value` contains at most `max` grapheme clusters.
pub fn validate_grapheme_max(value: &str, max: usize) -> Result<(), ValidationError> {
    let len = grapheme_len(value);
    if len > max {
        let mut err = validation_error(
            "grapheme_length",
            format!(
                "length must be at most {} grapheme clusters (got {})",
                max, len
            ),
        );
        err.add_param("max".into(), &max);
        err.add_param("value_len".into(), &len);
        return Err(err);
    }
    Ok(())
}

// Specific validators for `validator` derive macros.
// Each takes `&String` as required by `validator`.

/// `PostCommentRequest.content`: allow empty when media is present, enforce max 5000 graphemes.
pub fn validate_comment_content(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_max(value, 5000)
}

/// `UpdateCommentRequest.content`: 1–5000 graphemes.
pub fn validate_comment_content_update(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_length(value, 1, 5000)
}

/// Display name used in multiple DTOs: 1–50 graphemes.
pub fn validate_display_name(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_length(value, 1, 50)
}

/// Poll question: non-empty text, with no application maximum length.
pub fn validate_poll_question(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_min(value, 1)
}

/// Poll answer text: non-empty text, with no application maximum length.
pub fn validate_poll_answer_text(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_min(value, 1)
}

/// Location description (optional): 0–255 graphemes.
/// Called only when `Some`, so empty string (0 graphemes) is allowed.
pub fn validate_location_description(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_max(value, 255)
}

/// Reaction key: 1–32 graphemes.
pub fn validate_reaction_key(value: &str) -> Result<(), ValidationError> {
    validate_grapheme_length(value, 1, 32)
}

/// Helper for manual validation paths that need a `String` error.
pub fn ensure_grapheme_len(value: &str, min: usize, max: usize, field: &str) -> Result<(), String> {
    let len = grapheme_len(value);
    if len < min || len > max {
        return Err(format!(
            "{} must be between {} and {} grapheme clusters (got {})",
            field, min, max, len
        ));
    }
    Ok(())
}

pub fn ensure_grapheme_max(value: &str, max: usize, field: &str) -> Result<(), String> {
    let len = grapheme_len(value);
    if len > max {
        return Err(format!(
            "{} must be at most {} grapheme clusters (got {})",
            field, max, len
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_len_counts_combining_and_emoji() {
        assert_eq!(grapheme_len("a"), 1);
        assert_eq!(grapheme_len("é"), 1); // precomposed
        assert_eq!(grapheme_len("e\u{301}"), 1); // e + combining acute
        assert_eq!(grapheme_len("🇩🇪"), 1); // flag
        // ZWJ family: woman-woman-girl-boy
        assert_eq!(grapheme_len("👩‍👩‍👧‍👦"), 1);
        assert_eq!(grapheme_len("中"), 1);
        assert_eq!(grapheme_len("中中"), 2);
        // combining sequence: a + acute + flag should be 2? Actually each cluster separate
        assert_eq!(grapheme_len("e\u{301}🇩🇪"), 2);
    }

    #[test]
    fn validate_lengths_at_boundaries() {
        let s = "a".repeat(50);
        assert!(validate_display_name(&s).is_ok());
        let s2 = "a".repeat(51);
        assert!(validate_display_name(&s2).is_err());
        let s3 = "a".repeat(5000);
        assert!(validate_comment_content(&s3).is_ok());
        let s4 = "a".repeat(5001);
        assert!(validate_comment_content(&s4).is_err());
    }

    #[test]
    fn poll_text_fields_have_no_application_maximum() {
        // Only emptiness is rejected; there is no upper bound to hit.
        assert!(validate_poll_question("").is_err());
        assert!(validate_poll_question("q").is_ok());
        for len in [499, 500, 501, 5_000] {
            assert!(validate_poll_question(&"a".repeat(len)).is_ok());
            assert!(validate_poll_answer_text(&"a".repeat(len)).is_ok());
        }
        // Grapheme counting still applies to the minimum: a combining sequence
        // and a ZWJ emoji are single graphemes.
        assert!(validate_poll_question("e\u{301}").is_ok());
        assert!(validate_poll_question("👩‍👩‍👧‍👦").is_ok());
        assert!(validate_poll_question(" ").is_ok(), "trimming is semantic");

        assert!(validate_poll_answer_text("").is_err());
        assert!(validate_poll_answer_text("a").is_ok());
    }

    #[test]
    fn grapheme_min_counts_clusters() {
        assert!(validate_grapheme_min("a", 1).is_ok());
        assert!(validate_grapheme_min("", 1).is_err());
        assert!(validate_grapheme_min("e\u{301}", 1).is_ok());
        assert!(validate_grapheme_min("🇩🇪", 2).is_err());
        assert!(validate_grapheme_min("🇩🇪🇩🇪", 2).is_ok());
    }

    #[test]
    fn grapheme_helper_exact_boundaries() {
        let n_199 = "a".repeat(199);
        assert!(ensure_grapheme_len(&n_199, 1, 200, "field").is_ok());
        let n_200 = "a".repeat(200);
        assert!(ensure_grapheme_len(&n_200, 1, 200, "field").is_ok());
        let n_201 = "a".repeat(201);
        assert!(ensure_grapheme_len(&n_201, 1, 200, "field").is_err());

        // Counting is in graphemes, not bytes: 200 CJK characters are 600 bytes
        // and still accepted.
        let ch_200 = "中".repeat(200);
        assert!(ch_200.len() > 200);
        assert!(ensure_grapheme_len(&ch_200, 1, 200, "field").is_ok());
        assert!(ensure_grapheme_len(&"中".repeat(201), 1, 200, "field").is_err());
    }

    #[test]
    fn reaction_key_exact_boundaries() {
        let n_minus = "a".repeat(31);
        assert!(validate_reaction_key(&n_minus).is_ok());
        let n = "a".repeat(32);
        assert!(validate_reaction_key(&n).is_ok());
        let n_plus = "a".repeat(33);
        assert!(validate_reaction_key(&n_plus).is_err());

        // Unicode reaction key within 32 graphemes but >32 bytes should be accepted
        // Each flag is ~8 bytes but 1 grapheme; 32 flags = 256 bytes but 32 graphemes => ok
        let flag_32 = "🇩🇪".repeat(32);
        assert!(flag_32.len() > 32);
        assert_eq!(grapheme_len(&flag_32), 32);
        assert!(validate_reaction_key(&flag_32).is_ok());

        // 33 flags = 33 graphemes => rejected
        let flag_33 = "🇩🇪".repeat(33);
        assert_eq!(grapheme_len(&flag_33), 33);
        assert!(validate_reaction_key(&flag_33).is_err());

        // ZWJ emoji 32 => ok, 33 => err, each ZWJ is multiple bytes
        let zwj_32 = "👩‍👩‍👧‍👦".repeat(32);
        assert!(zwj_32.len() > 32);
        assert_eq!(grapheme_len(&zwj_32), 32);
        assert!(validate_reaction_key(&zwj_32).is_ok());
        let zwj_33 = "👩‍👩‍👧‍👦".repeat(33);
        assert!(validate_reaction_key(&zwj_33).is_err());

        // Combining marks
        let comb_32 = "e\u{301}".repeat(32);
        assert!(comb_32.len() > 32);
        assert_eq!(grapheme_len(&comb_32), 32);
        assert!(validate_reaction_key(&comb_32).is_ok());
        let comb_33 = "e\u{301}".repeat(33);
        assert!(validate_reaction_key(&comb_33).is_err());
    }

    #[test]
    fn display_name_and_comment_boundaries() {
        // display name 50
        assert!(validate_display_name(&"a".repeat(49)).is_ok());
        assert!(validate_display_name(&"a".repeat(50)).is_ok());
        assert!(validate_display_name(&"a".repeat(51)).is_err());
        // Chinese display name
        assert!(validate_display_name(&"中".repeat(50)).is_ok());
        assert!(validate_display_name(&"中".repeat(51)).is_err());
        // flag display name
        assert!(validate_display_name(&"🇩🇪".repeat(50)).is_ok());
        assert!(validate_display_name(&"🇩🇪".repeat(51)).is_err());
        // combining
        assert!(validate_display_name(&"e\u{301}".repeat(50)).is_ok());
        assert!(validate_display_name(&"e\u{301}".repeat(51)).is_err());

        // comment content 5000 (allow empty for post, but update requires 1)
        assert!(validate_comment_content("").is_ok()); // post allows empty
        assert!(validate_comment_content(&"a".repeat(5000)).is_ok());
        assert!(validate_comment_content(&"a".repeat(5001)).is_err());
        assert!(validate_comment_content_update(&"a".repeat(4999)).is_ok());
        assert!(validate_comment_content_update(&"a".repeat(5000)).is_ok());
        assert!(validate_comment_content_update(&"a".repeat(5001)).is_err());
        assert!(validate_comment_content_update("").is_err());
        // Chinese 5000
        assert!(validate_comment_content(&"中".repeat(5000)).is_ok());
        assert!(validate_comment_content(&"中".repeat(5001)).is_err());
        // ZWJ 5000
        let zwj_5000 = "👩‍👩‍👧‍👦".repeat(5000);
        assert_eq!(grapheme_len(&zwj_5000), 5000);
        assert!(validate_comment_content(&zwj_5000).is_ok());
        let zwj_5001 = "👩‍👩‍👧‍👦".repeat(5001);
        assert!(validate_comment_content(&zwj_5001).is_err());

        // location description 0-255
        assert!(validate_location_description("").is_ok());
        assert!(validate_location_description(&"a".repeat(255)).is_ok());
        assert!(validate_location_description(&"a".repeat(256)).is_err());
        assert!(validate_location_description(&"中".repeat(255)).is_ok());
        assert!(validate_location_description(&"中".repeat(256)).is_err());
        assert!(validate_location_description(&"🇩🇪".repeat(255)).is_ok());
        assert!(validate_location_description(&"🇩🇪".repeat(256)).is_err());
    }

    #[test]
    fn media_filename_grapheme_limits() {
        // 1-255 graphemes
        assert!(ensure_grapheme_len("", 1, 255, "filename").is_err());
        assert!(ensure_grapheme_len("a", 1, 255, "filename").is_ok());
        assert!(ensure_grapheme_len(&"a".repeat(255), 1, 255, "filename").is_ok());
        assert!(ensure_grapheme_len(&"a".repeat(256), 1, 255, "filename").is_err());
        assert!(ensure_grapheme_len(&"中".repeat(255), 1, 255, "filename").is_ok());
        assert!(ensure_grapheme_len(&"中".repeat(256), 1, 255, "filename").is_err());
        let flag_255 = "🇩🇪".repeat(255);
        assert_eq!(grapheme_len(&flag_255), 255);
        assert!(ensure_grapheme_len(&flag_255, 1, 255, "filename").is_ok());
        assert!(flag_255.len() > 255);
        let flag_256 = "🇩🇪".repeat(256);
        assert!(ensure_grapheme_len(&flag_256, 1, 255, "filename").is_err());
    }
}
