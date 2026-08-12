//! Config file discovery and loading.

use super::settings::Settings;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
