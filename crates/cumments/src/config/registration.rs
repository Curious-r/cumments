//! Validation of the AppService registration file against configuration.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::fs;

pub(super) struct RegistrationCheck<'a> {
    pub(super) id: &'a str,
    pub(super) url: &'a str,
    pub(super) as_token: &'a str,
    pub(super) hs_token: &'a str,
    pub(super) sender_localpart: &'a str,
    pub(super) server_name: &'a str,
}

#[derive(Debug, Deserialize)]
struct RegistrationFile {
    id: String,
    url: String,
    as_token: String,
    hs_token: String,
    sender_localpart: String,
    #[serde(default)]
    rate_limited: bool,
    namespaces: RegistrationNamespaces,
}

#[derive(Debug, Deserialize)]
struct RegistrationNamespaces {
    users: Vec<NamespaceRule>,
    aliases: Vec<NamespaceRule>,
    #[serde(default)]
    rooms: Vec<NamespaceRule>,
}

#[derive(Debug, Deserialize)]
struct NamespaceRule {
    exclusive: bool,
    regex: String,
}

pub(super) fn validate_registration_file(path: &str, check: &RegistrationCheck<'_>) -> Result<()> {
    let raw = fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read registration file {}: {}", path, e))?;
    let registration: RegistrationFile = serde_yaml_ng::from_str(&raw)
        .map_err(|e| anyhow!("failed to parse registration file {}: {}", path, e))?;

    let mut errors = Vec::new();

    if registration.id != check.id {
        errors.push(format!(
            "id: registration is '{}', config is '{}'",
            registration.id, check.id
        ));
    }
    if registration.url != check.url {
        errors.push(format!(
            "url: registration is '{}', config is '{}'",
            registration.url, check.url
        ));
    }
    if registration.as_token != check.as_token {
        errors.push("as_token does not match".to_string());
    }
    if registration.hs_token != check.hs_token {
        errors.push("hs_token does not match".to_string());
    }
    if registration.sender_localpart != check.sender_localpart {
        errors.push(format!(
            "sender_localpart: registration is '{}', config is '{}'",
            registration.sender_localpart, check.sender_localpart
        ));
    }

    if registration.rate_limited {
        errors.push("rate_limited must be false: the Cumments generator does not rate-limit appservice requests".to_string());
    }

    let expected_users = format!("@_cumments_.*:{}", regex_escape(check.server_name));
    match registration.namespaces.users.as_slice() {
        [rule] if rule.regex == expected_users && rule.exclusive => {}
        _ => errors.push(format!(
            "users namespace must be exactly `{expected_users}` with exclusive: true"
        )),
    }

    let expected_aliases = format!("#_cumments_.*:{}", regex_escape(check.server_name));
    match registration.namespaces.aliases.as_slice() {
        [rule] if rule.regex == expected_aliases && rule.exclusive => {}
        _ => errors.push(format!(
            "aliases namespace must be exactly `{expected_aliases}` with exclusive: true"
        )),
    }

    if !registration.namespaces.rooms.is_empty() {
        errors.push(
            "rooms namespace must be empty: Cumments reserves only users and aliases".to_string(),
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "registration file '{}' is inconsistent with this configuration:\n- {}",
            path,
            errors.join("\n- ")
        )
    }
}

/// Minimal regex escape for Matrix namespace patterns.
pub(crate) fn regex_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_REG_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn registration_file_validation_catches_drift() {
        let path = std::env::temp_dir().join(format!(
            "cumments-reg-{}-{}.yaml",
            std::process::id(),
            NEXT_REG_FILE.fetch_add(1, Ordering::SeqCst)
        ));
        let yaml = r#"
id: cumments
url: https://cumments.example.com
as_token: as
hs_token: hs
sender_localpart: _cumments_bot
rate_limited: false
namespaces:
  users:
  - exclusive: true
    regex: '@_cumments_.*:example\.com'
  aliases:
  - exclusive: true
    regex: '#_cumments_.*:example\.com'
  rooms: []
"#;
        std::fs::write(&path, yaml).expect("write registration");

        let check = RegistrationCheck {
            id: "cumments",
            url: "https://cumments.example.com",
            as_token: "as",
            hs_token: "hs",
            sender_localpart: "_cumments_bot",
            server_name: "example.com",
        };
        validate_registration_file(path.to_str().unwrap(), &check).expect("matching registration");

        let drifted = RegistrationCheck {
            hs_token: "different",
            ..check
        };
        let err = validate_registration_file(path.to_str().unwrap(), &drifted)
            .expect_err("drifted registration must fail");
        assert!(err.to_string().contains("hs_token does not match"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn regex_escape_leaves_plain_server_names_alone() {
        assert_eq!(regex_escape("curious.host"), "curious\\.host");
        assert_eq!(regex_escape("localhost"), "localhost");
        assert_eq!(regex_escape("example-123.com"), "example-123\\.com");
    }

    #[test]
    fn regex_escape_escapes_all_metacharacters() {
        assert_eq!(
            regex_escape(r"a+b(c).d[e]{1}|^$\\*?x"),
            r"a\+b\(c\)\.d\[e\]\{1\}\|\^\$\\\\\*\?x"
        );
    }
}
