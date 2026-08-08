use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::fs;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub server: Server,
    pub database: Database,
    pub security: Security,
    pub matrix: Matrix,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub cors_origins: String,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 7931,
            cors_origins: "*".to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    pub url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    pub pow_secret: String,
    pub pow_difficulty: u32,
}

/// Operation mode of the Matrix integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Appservice,
    Logging,
}

/// Matrix integration settings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matrix {
    pub mode: Mode,
    pub homeserver: Option<Homeserver>,
    pub appservice: Option<AppService>,
    pub moderation: Option<Moderation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Homeserver {
    /// Base URL of the homeserver CS API as seen by the AppService.
    pub address: Option<String>,
    /// Matrix ID domain (the part after `:` in user IDs and aliases).
    pub domain: Option<String>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppService {
    /// Must match the `id` in registration.yaml.
    pub id: String,
    /// URL the homeserver uses to reach this Cumments instance.
    pub url: Option<String>,
    /// Local bind address for the push receiver.
    pub listen_host: String,
    /// Local bind port for the push receiver.
    /// Equal to `server.port` to share the main HTTP listener.
    pub listen_port: u16,
    /// Localpart of the AppService sender user.
    pub sender_localpart: String,
    /// AppService token used to authenticate against the homeserver.
    pub as_token: Option<String>,
    /// Homeserver token used to verify incoming push transactions.
    pub hs_token: Option<String>,
    /// Optional path to registration.yaml; when set, startup validates that
    /// id/url/tokens/sender/namespaces match this configuration.
    pub registration_file: Option<String>,
}

impl Default for AppService {
    fn default() -> Self {
        Self {
            id: "cumments".to_string(),
            url: None,
            listen_host: "0.0.0.0".to_string(),
            listen_port: 3001,
            sender_localpart: "_cumments_bot".to_string(),
            as_token: None,
            hs_token: None,
            registration_file: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moderation {
    /// Matrix account invited to comment rooms with admin power.
    pub owner_id: Option<String>,
}

/// Fully validated AppService settings, ready to wire up the driver.
#[derive(Clone)]
pub struct AppServiceRuntime {
    pub id: String,
    pub homeserver_url: String,
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub sender_localpart: String,
    pub owner_id: String,
    pub listen_host: String,
    pub listen_port: u16,
    pub appservice_url: String,
}

impl Matrix {
    /// Validate all AppService-specific settings and return a ready-to-use bundle.
    pub fn appservice_runtime(&self) -> Result<AppServiceRuntime> {
        if self.mode != Mode::Appservice {
            bail!("`matrix.appservice_runtime` requires mode = \"appservice\"");
        }

        let homeserver = self
            .homeserver
            .as_ref()
            .ok_or_else(|| anyhow!("`[matrix.homeserver]` is required in appservice mode"))?;
        let homeserver_url = homeserver
            .address
            .as_ref()
            .ok_or_else(|| anyhow!("`matrix.homeserver.address` is required in appservice mode"))?;
        let server_name = homeserver
            .domain
            .as_ref()
            .ok_or_else(|| anyhow!("`matrix.homeserver.domain` is required in appservice mode"))?;

        let appservice = self
            .appservice
            .as_ref()
            .ok_or_else(|| anyhow!("`[matrix.appservice]` is required in appservice mode"))?;
        let appservice_url = appservice
            .url
            .as_ref()
            .ok_or_else(|| anyhow!("`matrix.appservice.url` is required in appservice mode"))?;
        let as_token = appservice.as_token.as_ref().ok_or_else(|| {
            anyhow!("`matrix.appservice.as_token` is required in appservice mode")
        })?;
        let hs_token = appservice.hs_token.as_ref().ok_or_else(|| {
            anyhow!("`matrix.appservice.hs_token` is required in appservice mode")
        })?;

        let owner_id = self
            .moderation
            .as_ref()
            .and_then(|m| m.owner_id.as_ref())
            .ok_or_else(|| {
                anyhow!("`matrix.moderation.owner_id` is required in appservice mode")
            })?;

        if !appservice.sender_localpart.starts_with("_cumments_") {
            bail!(
                "`matrix.appservice.sender_localpart` must start with `_cumments_` \
                 so the sender user falls inside the users namespace"
            );
        }

        if let Some(path) = &appservice.registration_file {
            validate_registration_file(
                path,
                &RegistrationCheck {
                    id: &appservice.id,
                    url: appservice_url,
                    as_token,
                    hs_token,
                    sender_localpart: &appservice.sender_localpart,
                    server_name,
                },
            )?;
        }

        Ok(AppServiceRuntime {
            id: appservice.id.clone(),
            homeserver_url: homeserver_url.clone(),
            server_name: server_name.clone(),
            as_token: as_token.clone(),
            hs_token: hs_token.clone(),
            sender_localpart: appservice.sender_localpart.clone(),
            owner_id: owner_id.clone(),
            listen_host: appservice.listen_host.clone(),
            listen_port: appservice.listen_port,
            appservice_url: appservice_url.clone(),
        })
    }
}

/// Reads configuration from a file and environment variables.
/// If `config_path` is provided, it loads that specific file.
/// Otherwise, it looks for `config.toml` in the current directory.
///
/// Environment variables use the `CUMMENTS__` prefix and `__` as the level
/// separator, e.g. `CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN`.
pub fn get_configuration(config_path: Option<&str>) -> Result<Settings, ::config::ConfigError> {
    let mut builder = ::config::Config::builder();

    if let Some(path) = config_path {
        builder = builder.add_source(::config::File::with_name(path).required(true));
    } else {
        builder = builder.add_source(::config::File::with_name("config").required(false));
    }

    let settings = builder
        .add_source(::config::Environment::with_prefix("CUMMENTS").separator("__"))
        .build()?;

    settings.try_deserialize()
}

struct RegistrationCheck<'a> {
    id: &'a str,
    url: &'a str,
    as_token: &'a str,
    hs_token: &'a str,
    sender_localpart: &'a str,
    server_name: &'a str,
}

#[derive(Debug, Deserialize)]
struct RegistrationFile {
    id: String,
    url: String,
    as_token: String,
    hs_token: String,
    sender_localpart: String,
    namespaces: RegistrationNamespaces,
}

#[derive(Debug, Deserialize)]
struct RegistrationNamespaces {
    users: Vec<NamespaceRule>,
    aliases: Vec<NamespaceRule>,
}

#[derive(Debug, Deserialize)]
struct NamespaceRule {
    exclusive: bool,
    regex: String,
}

fn validate_registration_file(path: &str, check: &RegistrationCheck<'_>) -> Result<()> {
    let raw = fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read registration file {}: {}", path, e))?;
    let registration: RegistrationFile = serde_yaml::from_str(&raw)
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

    fn parse(toml: &str) -> Result<Settings, ::config::ConfigError> {
        let builder = ::config::Config::builder()
            .add_source(::config::File::from_str(toml, ::config::FileFormat::Toml))
            .build()?;
        builder.try_deserialize()
    }

    #[test]
    fn parses_nested_appservice_config() {
        let settings = parse(
            r#"
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
pow_secret = "secret"
pow_difficulty = 4

[matrix]
mode = "appservice"

[matrix.homeserver]
address = "https://matrix.example.com"
domain = "example.com"

[matrix.appservice]
id = "cumments"
url = "https://cumments.example.com"
listen_host = "0.0.0.0"
listen_port = 3001
sender_localpart = "_cumments_bot"
as_token = "as"
hs_token = "hs"

[matrix.moderation]
owner_id = "@admin:example.com"
"#,
        )
        .expect("config should parse");

        assert_eq!(settings.matrix.mode, Mode::Appservice);
        let runtime = settings
            .matrix
            .appservice_runtime()
            .expect("valid appservice");
        assert_eq!(runtime.homeserver_url, "https://matrix.example.com");
        assert_eq!(runtime.server_name, "example.com");
        assert_eq!(runtime.listen_port, 3001);
    }

    #[test]
    fn logging_mode_needs_only_the_matrix_mode() {
        let settings = parse(
            r#"
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
pow_secret = "secret"
pow_difficulty = 4

[matrix]
mode = "logging"
"#,
        )
        .expect("logging config should parse");

        assert_eq!(settings.matrix.mode, Mode::Logging);
        assert!(settings.matrix.appservice.is_none());
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = parse(
            r#"
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
pow_secret = "secret"
pow_difficulty = 4

[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008"
server_name = "example.com"
as_token = "as"
hs_token = "hs"
owner_id = "@admin:example.com"
"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn appservice_requires_nested_sections() {
        let settings = parse(
            r#"
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
pow_secret = "secret"
pow_difficulty = 4

[matrix]
mode = "appservice"
"#,
        )
        .expect("config should parse");

        let err = match settings.matrix.appservice_runtime() {
            Ok(_) => panic!("appservice mode without homeserver must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("[matrix.homeserver]"));
    }

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
