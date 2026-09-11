//! Canonical JSON version-1 serialization (frozen Poll design §9.2).
//!
//! The signed representation and the semantic-operation fingerprint use
//! exactly one byte sequence per value. Version 1 permits only arrays,
//! strings, integers, booleans and `null`; JSON objects are deliberately
//! excluded, so there is no property-order ambiguity and RFC 8785 (JCS) is
//! not needed.
//!
//! The rules are:
//! * compact UTF-8, no whitespace outside string literals;
//! * `"` and `\` escaped, `\b` `\f` `\n` `\r` `\t` short escapes, every other
//!   `U+0000..U+001F` control as lowercase `\u00xx`;
//! * `/` is never escaped;
//! * other scalar values are emitted literally (no Unicode normalization,
//!   no case folding, no `\u` escapes for ordinary non-ASCII);
//! * integers are plain base-10 with no leading zero, `+`, decimal point or
//!   exponent.

/// A value in the version-1 canonical JSON representation.
///
/// The enum deliberately has no object variant: version 1 excludes JSON
/// objects from the signed representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalJson {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<CanonicalJson>),
}

impl CanonicalJson {
    /// A string value.
    pub fn string(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// An integer value.
    pub fn int(value: i64) -> Self {
        Self::Int(value)
    }

    /// An array value.
    pub fn array(values: Vec<CanonicalJson>) -> Self {
        Self::Array(values)
    }

    /// A string value from an optional value; `None` becomes JSON `null`.
    pub fn nullable_string(value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => Self::Str(value.into()),
            None => Self::Null,
        }
    }

    /// The single canonical UTF-8 byte sequence for this value.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        self.write(&mut out);
        out.into_bytes()
    }

    /// The single canonical UTF-8 string for this value.
    pub fn to_canonical_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The equivalent `serde_json::Value`, used when embedding the canonical
    /// value in Matrix provenance. The produced JSON has the same structure
    /// as the canonical form.
    pub fn to_json_value(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Int(value) => serde_json::Value::Number((*value).into()),
            Self::Str(value) => serde_json::Value::String(value.clone()),
            Self::Array(values) => {
                serde_json::Value::Array(values.iter().map(Self::to_json_value).collect())
            }
        }
    }

    /// Recover a canonical value from parsed JSON.
    ///
    /// Returns `None` for values outside the version-1 type set (objects,
    /// floats, or integers that do not fit `i64`). Used to re-verify a
    /// signature over the semantic operation recovered from Matrix provenance.
    pub fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::Null => Some(Self::Null),
            serde_json::Value::Bool(value) => Some(Self::Bool(*value)),
            serde_json::Value::Number(number) => number.as_i64().map(Self::Int),
            serde_json::Value::String(value) => Some(Self::Str(value.clone())),
            serde_json::Value::Array(values) => values
                .iter()
                .map(Self::from_json_value)
                .collect::<Option<Vec<_>>>()
                .map(Self::Array),
            serde_json::Value::Object(_) => None,
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(true) => out.push_str("true"),
            Self::Bool(false) => out.push_str("false"),
            Self::Int(value) => out.push_str(&value.to_string()),
            Self::Str(value) => write_string(value, out),
            Self::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.write(out);
                }
                out.push(']');
            }
        }
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn write_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str("\\u00");
                out.push(HEX[((code >> 4) & 0xF) as usize] as char);
                out.push(HEX[(code & 0xF) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_serialize_compactly() {
        assert_eq!(CanonicalJson::Null.to_canonical_string(), "null");
        assert_eq!(CanonicalJson::Bool(true).to_canonical_string(), "true");
        assert_eq!(CanonicalJson::Bool(false).to_canonical_string(), "false");
        assert_eq!(CanonicalJson::int(0).to_canonical_string(), "0");
        assert_eq!(CanonicalJson::int(1).to_canonical_string(), "1");
        assert_eq!(CanonicalJson::int(42).to_canonical_string(), "42");
        assert_eq!(CanonicalJson::int(-7).to_canonical_string(), "-7");
        assert_eq!(CanonicalJson::string("hi").to_canonical_string(), "\"hi\"");
    }

    #[test]
    fn arrays_preserve_order_without_whitespace() {
        let value = CanonicalJson::array(vec![
            CanonicalJson::string("a"),
            CanonicalJson::int(1),
            CanonicalJson::Null,
        ]);
        assert_eq!(value.to_canonical_string(), r#"["a",1,null]"#);
        // Reordered arrays are a different canonical value.
        let reordered = CanonicalJson::array(vec![
            CanonicalJson::int(1),
            CanonicalJson::string("a"),
            CanonicalJson::Null,
        ]);
        assert_ne!(value, reordered);
    }

    #[test]
    fn nested_arrays_serialize_recursively() {
        let value = CanonicalJson::array(vec![
            CanonicalJson::string("POLL"),
            CanonicalJson::array(vec![CanonicalJson::string("my-blog"), CanonicalJson::Null]),
        ]);
        assert_eq!(value.to_canonical_string(), r#"["POLL",["my-blog",null]]"#);
    }

    #[test]
    fn required_escapes_use_short_forms() {
        let value = CanonicalJson::string("\"\\\u{0008}\u{000C}\n\r\t");
        assert_eq!(value.to_canonical_string(), r#""\"\\\b\f\n\r\t""#);
    }

    #[test]
    fn other_controls_use_lowercase_hex() {
        assert_eq!(
            CanonicalJson::string("\u{0000}\u{0001}\u{001F}").to_canonical_string(),
            r#""\u0000\u0001\u001f""#
        );
    }

    #[test]
    fn solidus_is_not_escaped_and_unicode_is_literal() {
        assert_eq!(
            CanonicalJson::string("a/b").to_canonical_string(),
            "\"a/b\""
        );
        // Ordinary non-ASCII is emitted literally, no NFC/NFD normalization.
        assert_eq!(
            CanonicalJson::string("é中").to_canonical_string(),
            "\"é中\""
        );
        assert_eq!(
            CanonicalJson::string("e\u{301}").to_canonical_string(),
            "\"e\u{301}\""
        );
    }

    #[test]
    fn canonical_bytes_are_valid_utf8_and_stable() {
        let value = CanonicalJson::array(vec![
            CanonicalJson::string("Which meeting time works best?"),
            CanonicalJson::array(vec![CanonicalJson::array(vec![
                CanonicalJson::string("slot-10am"),
                CanonicalJson::string("10:00 AM UTC"),
            ])]),
            CanonicalJson::string("disclosed"),
            CanonicalJson::int(1),
            CanonicalJson::int(1),
        ]);
        let bytes = value.to_canonical_bytes();
        assert_eq!(
            String::from_utf8(bytes.clone()).expect("utf8"),
            value.to_canonical_string()
        );
        assert_eq!(value.to_canonical_bytes(), bytes);
    }
}
