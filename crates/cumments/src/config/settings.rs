//! Typed settings model and defaults.

use super::registration::{RegistrationCheck, validate_registration_file};
use anyhow::{Result, anyhow, bail};
use cumments_core::site_auth::{SiteAuthMode, SiteVerificationPolicy};
use serde::Deserialize;
use std::collections::HashMap;

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
    /// Reverse proxies that are allowed to set `X-Forwarded-For`. Rate
    /// limiting only trusts the header when the peer is in this list;
    /// otherwise the peer IP is used as the client key.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 7931,
            trusted_proxies: Vec::new(),
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
    /// Allow verification of IP-literal origins in loopback/private/link-local
    /// address space. Off by default because confirm performs outbound probes.
    #[serde(default)]
    pub allow_private_verification_origins: bool,
    /// Preset sticker MXC URIs guests may reference in sticker messages.
    /// Stickers are served by the homeserver; Cumments only validates the
    /// reference.
    #[serde(default)]
    pub preset_stickers: Vec<String>,
    /// Independent HMAC key for signed media-proxy URLs. When unset the
    /// AppService token is used, so rotating the AS token invalidates
    /// outstanding media URLs; production deployments should set this.
    #[serde(default)]
    pub media_sign_key: Option<String>,
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

/// Fully validated AppService settings, ready to wire up the driver.
#[derive(Clone, Debug)]
pub struct AppServiceRuntime {
    pub id: String,
    pub homeserver_url: String,
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub sender_localpart: String,
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
            listen_host: listen_host.to_string(),
            listen_port: appservice.listen_port,
            room_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
