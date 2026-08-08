use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

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
#[derive(Clone, Debug)]
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

fn require_non_empty<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("`{field}` must not be empty"),
    }
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
        let homeserver_url =
            require_non_empty(homeserver.address.as_deref(), "matrix.homeserver.address")?;
        let server_name =
            require_non_empty(homeserver.domain.as_deref(), "matrix.homeserver.domain")?;

        let appservice = self
            .appservice
            .as_ref()
            .ok_or_else(|| anyhow!("`[matrix.appservice]` is required in appservice mode"))?;
        let appservice_url = require_non_empty(appservice.url.as_deref(), "matrix.appservice.url")?;
        let as_token =
            require_non_empty(appservice.as_token.as_deref(), "matrix.appservice.as_token")?;
        let hs_token =
            require_non_empty(appservice.hs_token.as_deref(), "matrix.appservice.hs_token")?;
        let owner_id = require_non_empty(
            self.moderation.as_ref().and_then(|m| m.owner_id.as_deref()),
            "matrix.moderation.owner_id",
        )?;
        let id = require_non_empty(Some(&appservice.id), "matrix.appservice.id")?;
        let sender_localpart = require_non_empty(
            Some(&appservice.sender_localpart),
            "matrix.appservice.sender_localpart",
        )?;
        let listen_host = require_non_empty(
            Some(&appservice.listen_host),
            "matrix.appservice.listen_host",
        )?;

        if !sender_localpart.starts_with("_cumments_") {
            bail!(
                "`matrix.appservice.sender_localpart` must start with `_cumments_` \
                 so the sender user falls inside the users namespace"
            );
        }

        if let Some(path) = &appservice.registration_file {
            validate_registration_file(
                path,
                &RegistrationCheck {
                    id,
                    url: appservice_url,
                    as_token,
                    hs_token,
                    sender_localpart,
                    server_name,
                },
            )?;
        }

        Ok(AppServiceRuntime {
            id: id.to_string(),
            homeserver_url: homeserver_url.to_string(),
            server_name: server_name.to_string(),
            as_token: as_token.to_string(),
            hs_token: hs_token.to_string(),
            sender_localpart: sender_localpart.to_string(),
            owner_id: owner_id.to_string(),
            listen_host: listen_host.to_string(),
            listen_port: appservice.listen_port,
            appservice_url: appservice_url.to_string(),
        })
    }
}

/// Resolve the configuration file path.
///
/// Priority:
/// 1. `--config <path>` (explicit CLI flag)
/// 2. `CUMMENTS_CONFIG` environment variable
/// 3. `$XDG_CONFIG_HOME/cumments/cumments.toml` (or `~/.config/cumments/cumments.toml`)
/// 4. `/etc/cumments/cumments.toml`
/// 5. `./cumments.toml` (local development fallback)
pub fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var("CUMMENTS_CONFIG") {
        return Some(PathBuf::from(path));
    }

    default_config_paths()
        .into_iter()
        .find(|path| path.exists())
}

fn default_config_paths() -> Vec<PathBuf> {
    config_paths(
        valid_config_dir(std::env::var_os("XDG_CONFIG_HOME")),
        valid_config_dir(std::env::var_os("HOME")),
    )
}

/// Builds the user, system, and local fallback paths in discovery order.
///
/// Per the XDG Base Directory Specification, `XDG_CONFIG_HOME` takes
/// precedence; when it is unset, empty, or relative, `~/.config` is used.
fn config_paths(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(xdg) = xdg_config_home {
        paths.push(xdg.join("cumments").join("cumments.toml"));
    } else if let Some(home) = home {
        paths.push(home.join(".config").join("cumments").join("cumments.toml"));
    }

    paths.push(PathBuf::from("/etc/cumments/cumments.toml"));
    paths.push(PathBuf::from("cumments.toml"));
    paths
}

/// Accepts a config directory only when it is non-empty and absolute.
fn valid_config_dir(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Reads configuration from a file and environment variables.
/// File discovery follows [`resolve_config_path`]; environment variables use
/// the `CUMMENTS__` prefix and `__` as the level separator, e.g.
/// `CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN`, and override file values.
pub fn get_configuration(config_path: Option<&str>) -> Result<Settings, ::config::ConfigError> {
    let mut builder = ::config::Config::builder();

    if let Some(path) = resolve_config_path(config_path) {
        builder = builder.add_source(::config::File::from(path).required(true));
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

    #[test]
    fn explicit_config_path_takes_precedence() {
        assert_eq!(
            resolve_config_path(Some("custom.toml")),
            Some(PathBuf::from("custom.toml"))
        );
    }

    #[test]
    fn default_config_paths_include_system_and_local_fallbacks() {
        let paths = default_config_paths();
        assert!(paths.contains(&PathBuf::from("/etc/cumments/cumments.toml")));
        assert!(paths.contains(&PathBuf::from("cumments.toml")));
    }

    #[test]
    fn user_config_prefers_valid_xdg_config_home() {
        let paths = config_paths(
            Some(PathBuf::from("/xdg")),
            Some(PathBuf::from("/home/user")),
        );
        assert!(paths.contains(&PathBuf::from("/xdg/cumments/cumments.toml")));
        assert!(!paths.contains(&PathBuf::from("/home/user/.config/cumments/cumments.toml")));
    }

    #[test]
    fn user_config_falls_back_to_home_config() {
        let paths = config_paths(None, Some(PathBuf::from("/home/user")));
        assert!(paths.contains(&PathBuf::from("/home/user/.config/cumments/cumments.toml")));
    }

    #[test]
    fn empty_or_relative_config_dirs_are_rejected() {
        assert_eq!(valid_config_dir(Some(std::ffi::OsString::from(""))), None);
        assert_eq!(
            valid_config_dir(Some(std::ffi::OsString::from("relative"))),
            None
        );
        assert_eq!(
            valid_config_dir(Some(std::ffi::OsString::from("/absolute"))),
            Some(PathBuf::from("/absolute"))
        );
    }

    #[test]
    fn appservice_runtime_rejects_empty_values() {
        let mut settings = parse(
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
        .expect("parse settings");

        settings.matrix.appservice.as_mut().unwrap().url = Some(String::new());
        let err = settings.matrix.appservice_runtime().expect_err("empty url");
        assert!(err.to_string().contains("matrix.appservice.url"));

        settings.matrix.appservice.as_mut().unwrap().url =
            Some("https://cumments.example.com".to_string());
        settings.matrix.appservice.as_mut().unwrap().as_token = Some(String::new());
        let err = settings
            .matrix
            .appservice_runtime()
            .expect_err("empty as_token");
        assert!(err.to_string().contains("matrix.appservice.as_token"));

        settings.matrix.appservice.as_mut().unwrap().as_token = Some("as".to_string());
        settings.matrix.moderation.as_mut().unwrap().owner_id = Some(String::new());
        let err = settings
            .matrix
            .appservice_runtime()
            .expect_err("empty owner_id");
        assert!(err.to_string().contains("matrix.moderation.owner_id"));
    }
}
