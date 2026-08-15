//! Configuration: typed settings, file discovery, site-auth policy and
//! AppService registration-file validation.

mod paths;
mod policy;
mod registration;
mod settings;

pub use paths::{get_configuration, resolve_config_path};
pub use policy::{
    build_site_auth_policy, is_known_pow_placeholder, operator_token_hash, validate_operator_mxids,
    validate_pow_secret,
};
pub(crate) use registration::regex_escape;
pub use settings::{
    AppService, AppServiceRuntime, Database, Homeserver, Matrix, Mode, Security, Server, Settings,
    SiteConfig,
};
