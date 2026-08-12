//! Configuration: typed settings, file discovery, site-auth policy and
//! AppService registration-file validation.

mod paths;
mod policy;
mod registration;
mod settings;

pub use paths::{get_configuration, resolve_config_path};
pub use policy::{
    admin_token_hash, build_site_auth_policy, is_known_pow_placeholder, validate_pow_secret,
};
pub(crate) use registration::regex_escape;
pub use settings::{
    AppService, AppServiceRuntime, Database, Homeserver, Matrix, Mode, Moderation, Security,
    Server, Settings, SiteConfig,
};
