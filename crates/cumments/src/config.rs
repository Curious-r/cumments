use anyhow::{Result, anyhow, bail};
use cumments_core::site_auth::{
    KNOWN_SECRET_PLACEHOLDERS, OriginPattern, SITE_SECRET_MIN_LENGTH, SiteAuthMode, SiteAuthPolicy,
    SitePolicyEntry, SiteVerificationPolicy,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub server: Server,
    pub database: Database,
    pub security: Security,
    pub matrix: Matrix,
    /// Operator-declared trust for individual sites (the config overlay).
    #[serde(default)]
    pub sites: HashMap<String, SiteConfig>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub host: String,
    pub port: u16,
    #[serde(rename = "cors_origins", default)]
    pub legacy_cors_origins: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 7931,
            legacy_cors_origins: None,
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
    /// Instance-wide policy for site verification.
    #[serde(default)]
    pub site_verification: SiteVerificationPolicy,
    /// Operator token for the admin API. When unset, admin routes return 403.
    #[serde(default)]
    pub admin_token: Option<String>,
}

/// Operator-declared trust for one site.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SiteConfig {
    /// How this site authenticates write requests (`origin` or `secret`).
    pub auth_mode: Option<SiteAuthMode>,
    /// Exact origins or `https://*.example.com` subdomain wildcards that are
    /// trusted without online verification.
    pub allowed_origins: Vec<String>,
    /// HMAC secret for `auth_mode = "secret"`. Prefer injecting this through
    /// `CUMMENTS__SITES__<site_id>__SECRET`; never commit it.
    pub secret: Option<String>,
}

/// Operation mode of the Matrix integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    AppService,
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
    /// Room version to request when creating rooms, e.g. `"12"`.
    /// When unset, the homeserver's configured default is used.
    pub room_version: Option<String>,
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
            room_version: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moderation {
    /// Matrix account invited to comment rooms with admin power.
    pub admin_id: Option<String>,
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
    pub admin_id: String,
    pub listen_host: String,
    pub listen_port: u16,
    pub room_version: Option<String>,
}

fn require_non_empty<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("`{field}` must not be empty"),
    }
}

/// Rejects the removed `server.cors_origins` key with an explicit message.
pub fn validate_legacy_cors(server: &Server) -> Result<()> {
    if let Some(value) = &server.legacy_cors_origins {
        bail!(
            "`server.cors_origins` has been removed (value `{value}`): CORS is now derived \
             from site verification and the `[sites]` allowlist. Remove the key from your \
             configuration."
        );
    }
    Ok(())
}

/// Builds the effective site-auth policy from the configuration, validating
/// and normalizing every operator-declared entry.
pub fn build_site_auth_policy(
    security: &Security,
    sites: &HashMap<String, SiteConfig>,
) -> Result<SiteAuthPolicy> {
    let mut policy = SiteAuthPolicy {
        verification: security.site_verification,
        sites: HashMap::new(),
    };

    for (site_id, config) in sites {
        let allowed_origins = config
            .allowed_origins
            .iter()
            .map(|raw| {
                OriginPattern::parse(raw).map_err(|e| {
                    anyhow!("`sites.{site_id}.allowed_origins` entry `{raw}` is invalid: {e}")
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let secret = match (&config.auth_mode, &config.secret) {
            (Some(SiteAuthMode::Secret), Some(secret)) => {
                if secret.len() < SITE_SECRET_MIN_LENGTH {
                    bail!(
                        "`sites.{site_id}.secret` must be at least {SITE_SECRET_MIN_LENGTH} \
                         characters (it is an HMAC key, not a label)"
                    );
                }
                if KNOWN_SECRET_PLACEHOLDERS.contains(&secret.as_str()) {
                    bail!(
                        "`sites.{site_id}.secret` uses the known example value `{secret}`; \
                         generate a real random secret"
                    );
                }
                Some(secret.clone())
            }
            (Some(SiteAuthMode::Secret), None) => {
                bail!(
                    "`sites.{site_id}.auth_mode = \"secret\"` requires a secret; set \
                     `CUMMENTS__SITES__{site_id}__SECRET` or add `secret` to the site config"
                );
            }
            (None | Some(SiteAuthMode::Origin), Some(_)) => {
                bail!(
                    "`sites.{site_id}.secret` is set but `auth_mode` is not `\"secret\"`; \
                     the secret would be ignored"
                );
            }
            (None | Some(SiteAuthMode::Origin), None) => None,
        };

        if config.auth_mode == Some(SiteAuthMode::Secret) && !allowed_origins.is_empty() {
            tracing::warn!(
                "`sites.{site_id}.allowed_origins` is ignored because \
                 `auth_mode = \"secret\"` authenticates with the HMAC key"
            );
        }

        for pattern in &allowed_origins {
            warn_http_non_loopback(site_id, pattern);
        }

        policy.sites.insert(
            site_id.clone(),
            SitePolicyEntry {
                auth_mode: config.auth_mode,
                allowed_origins,
                secret,
            },
        );
    }

    Ok(policy)
}

/// Warns about plain-HTTP origin patterns that are not loopback addresses.
fn warn_http_non_loopback(site_id: &str, pattern: &OriginPattern) {
    let (scheme, host) = match pattern {
        OriginPattern::Exact(origin) => {
            let Ok(url) = origin.as_str().parse::<url::Url>() else {
                return;
            };
            (url.scheme().to_string(), url.host_str().map(str::to_owned))
        }
        OriginPattern::Wildcard {
            scheme,
            host_suffix,
            ..
        } => (scheme.clone(), Some(host_suffix.clone())),
    };
    if scheme == "http"
        && !matches!(
            host.as_deref(),
            Some("localhost") | Some("127.0.0.1") | Some("::1")
        )
    {
        tracing::warn!(
            "`sites.{site_id}.allowed_origins` allows plain HTTP origin `{}`; \
             use HTTPS in production",
            pattern.as_pattern_string()
        );
    }
}

/// Validates the admin token and returns its SHA-256 hash for comparison.
pub fn admin_token_hash(security: &Security) -> Result<Option<String>> {
    let Some(token) = &security.admin_token else {
        return Ok(None);
    };
    if token.trim().is_empty() {
        bail!("`security.admin_token` must not be empty");
    }
    if token.len() < 32 {
        bail!("`security.admin_token` must be at least 32 characters");
    }
    if matches!(token.as_str(), "change-me" | "admin-token") {
        bail!("`security.admin_token` uses a known example value; generate a real random token");
    }
    Ok(Some(cumments_core::site_auth::token_hash(token)))
}

/// Known example/placeholder secrets shipped in the repository. They are
/// harmless in `logging` mode but would let anyone forge PoW challenges in
/// production.
pub fn is_known_pow_placeholder(secret: &str) -> bool {
    matches!(secret, "change-me" | "pow_secret_key")
}

/// Validate the PoW secret: it must never be empty, and in AppService mode it
/// must not be a publicly known placeholder.
pub fn validate_pow_secret(secret: &str, mode: Mode) -> Result<()> {
    if secret.trim().is_empty() {
        bail!("`security.pow_secret` must not be empty");
    }
    if mode == Mode::AppService && is_known_pow_placeholder(secret) {
        bail!(
            "`security.pow_secret` uses the known example value `{}`; \
             set a real random secret before running in appservice mode",
            secret
        );
    }
    Ok(())
}

impl Matrix {
    /// Validate all AppService-specific settings and return a ready-to-use bundle.
    pub fn appservice_runtime(&self) -> Result<AppServiceRuntime> {
        if self.mode != Mode::AppService {
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
        let admin_id = require_non_empty(
            self.moderation.as_ref().and_then(|m| m.admin_id.as_deref()),
            "matrix.moderation.admin_id",
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

        let room_version = appservice
            .room_version
            .as_deref()
            .map(|v| {
                if v.is_empty()
                    || v.chars().count() > 32
                    || !v.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-')
                    })
                {
                    bail!(
                        "`matrix.appservice.room_version` must be a valid Matrix room version \
                         (1-32 chars from a-z, 0-9, '.', '-')"
                    );
                }
                Ok(v.to_string())
            })
            .transpose()?;

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
            admin_id: admin_id.to_string(),
            listen_host: listen_host.to_string(),
            listen_port: appservice.listen_port,
            room_version,
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
admin_id = "@admin:example.com"
"#,
        )
        .expect("config should parse");

        assert_eq!(settings.matrix.mode, Mode::AppService);
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
bogus_option = "x"

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
admin_id = "@admin:example.com"
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
admin_id = "@admin:example.com"
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
        settings.matrix.moderation.as_mut().unwrap().admin_id = Some(String::new());
        let err = settings
            .matrix
            .appservice_runtime()
            .expect_err("empty admin_id");
        assert!(err.to_string().contains("matrix.moderation.admin_id"));
    }

    #[test]
    fn pow_secret_validation_rejects_empty_and_placeholders_in_appservice() {
        assert!(validate_pow_secret("", Mode::Logging).is_err());
        assert!(validate_pow_secret("   ", Mode::AppService).is_err());
        assert!(validate_pow_secret("change-me", Mode::AppService).is_err());
        assert!(validate_pow_secret("pow_secret_key", Mode::AppService).is_err());
        // The runnable example stays usable in logging mode.
        assert!(validate_pow_secret("change-me", Mode::Logging).is_ok());
        assert!(validate_pow_secret("a-real-secret", Mode::AppService).is_ok());
    }

    #[test]
    fn site_verification_policy_parses_all_values() {
        let toml = r#"
[server]
host = "localhost"
port = 7931

[database]
url = "sqlite://data/cumments.db"

[security]
pow_secret = "secret"
pow_difficulty = 4
site_verification = "required"

[matrix]
mode = "logging"
"#;
        let settings = parse(toml).expect("parse settings");
        assert_eq!(
            settings.security.site_verification,
            cumments_core::site_auth::SiteVerificationPolicy::Required
        );

        let missing = toml.replace("site_verification = \"required\"\n", "");
        let settings = parse(&missing).expect("policy defaults to optional");
        assert_eq!(
            settings.security.site_verification,
            cumments_core::site_auth::SiteVerificationPolicy::Optional
        );
    }

    #[test]
    fn site_policy_build_normalizes_origins_and_validates_secrets() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
        };

        let mut sites = HashMap::new();
        sites.insert(
            "my-blog".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec![
                    "https://Blog.Example.com".to_string(),
                    "https://*.example.net".to_string(),
                ],
                secret: None,
            },
        );
        let policy = build_site_auth_policy(&security, &sites).expect("valid policy");
        let entry = policy.entry("my-blog").expect("entry exists");
        assert_eq!(entry.allowed_origins.len(), 2);

        sites.insert(
            "secret-site".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: Some("a".repeat(SITE_SECRET_MIN_LENGTH)),
            },
        );
        let policy = build_site_auth_policy(&security, &sites).expect("secret site valid");
        assert_eq!(
            policy
                .entry("secret-site")
                .and_then(|e| e.secret.as_deref()),
            Some("a".repeat(SITE_SECRET_MIN_LENGTH).as_str())
        );
    }

    #[test]
    fn site_policy_build_rejects_invalid_combinations() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
        };

        let mut sites = HashMap::new();
        sites.insert(
            "bad-origin".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec!["https://example.com/path".to_string()],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "no-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "short-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: Some("short".to_string()),
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "ignored-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Origin),
                allowed_origins: vec![],
                secret: Some("a".repeat(SITE_SECRET_MIN_LENGTH)),
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());
    }

    #[test]
    fn admin_token_hash_validates_and_hashes() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
        };
        assert!(
            admin_token_hash(&security)
                .expect("no token yields none")
                .is_none()
        );

        let mut security = security;
        security.admin_token = Some("short".to_string());
        assert!(admin_token_hash(&security).is_err());

        security.admin_token = Some("change-me".to_string());
        assert!(admin_token_hash(&security).is_err());

        security.admin_token = Some("a-very-long-admin-token-0123456789".to_string());
        let hash = admin_token_hash(&security)
            .expect("valid token")
            .expect("some hash");
        assert_eq!(
            hash,
            cumments_core::site_auth::token_hash("a-very-long-admin-token-0123456789")
        );
    }
}
