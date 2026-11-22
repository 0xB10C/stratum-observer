use serde::Deserialize;
use std::collections::BTreeSet;
use std::error;
use std::io;
use std::{env, fmt, fs};

pub const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";

use crate::types::Pool;
use log::info;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    pub postgresql_url: Option<String>,
    pub websocket_address: Option<String>,
    pub pools: Vec<Pool>,
}

impl Config {
    /// Parses a configuration from a TOML string and checks that it makes sense.
    pub fn from_toml_str(config_string: &str) -> Result<Config, ConfigError> {
        let mut config: Config = toml::from_str(config_string)?;
        config.postgresql_url = non_empty(config.postgresql_url);
        config.websocket_address = non_empty(config.websocket_address);
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), ConfigError> {
        // there is nothing to observe without pools
        if self.pools.is_empty() {
            return Err(ConfigError::NoPools);
        }

        // check that every pool has a name and an endpoint
        for pool in self.pools.iter() {
            if pool.name.trim().is_empty() {
                return Err(ConfigError::MissingPoolName);
            }
            if pool.endpoint.trim().is_empty() {
                return Err(ConfigError::MissingPoolEndpoint(pool.name.clone()));
            }
        }

        // check for unique pool names
        let mut pool_names = BTreeSet::new();
        for pool in self.pools.iter() {
            if !pool_names.insert(&pool.name) {
                return Err(ConfigError::DuplicatePoolName(pool.name.clone()));
            }
        }
        Ok(())
    }
}

/// An empty or blank setting means the same as leaving it out: the feature
/// it belongs to is turned off.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Reads the configuration from the file at the given path.
pub fn load_config_from_path(path: &str) -> Result<Config, ConfigError> {
    info!("Reading configuration file from {}.", path);
    let config_string = fs::read_to_string(path)?;
    Config::from_toml_str(&config_string)
}

/// Reads the configuration from the file the CONFIG_FILE environment variable
/// points to, falling back to config.toml.
pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    load_config_from_path(&config_file_path)
}

#[derive(Debug)]
pub enum ConfigError {
    TomlError(toml::de::Error),
    ReadError(io::Error),
    DuplicatePoolName(String),
    NoPools,
    MissingPoolName,
    MissingPoolEndpoint(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::TomlError(e) => write!(
                f,
                "the TOML in the configuration file could not be parsed: {}",
                e
            ),
            ConfigError::ReadError(e) => {
                write!(f, "the configuration file could not be read: {}", e)
            }
            ConfigError::DuplicatePoolName(name) => {
                write!(f, "duplicate pool name: {}", name)
            }
            ConfigError::NoPools => {
                write!(f, "the configuration does not list any pools")
            }
            ConfigError::MissingPoolName => {
                write!(f, "a pool does not have a name")
            }
            ConfigError::MissingPoolEndpoint(name) => {
                write!(f, "the pool {} does not have an endpoint", name)
            }
        }
    }
}

impl error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ConfigError::TomlError(ref e) => Some(e),
            ConfigError::ReadError(ref e) => Some(e),
            ConfigError::DuplicatePoolName(_)
            | ConfigError::NoPools
            | ConfigError::MissingPoolName
            | ConfigError::MissingPoolEndpoint(_) => None,
        }
    }
}

impl From<io::Error> for ConfigError {
    fn from(err: io::Error) -> ConfigError {
        ConfigError::ReadError(err)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(err: toml::de::Error) -> ConfigError {
        ConfigError::TomlError(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FILENAME_EXAMPLE_CONFIG: &str = "config-example.toml";

    /// Writes a configuration file into the temporary directory and returns
    /// its path. The file name is unique, so tests can run in parallel.
    fn write_config_file(contents: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let mut path = env::temp_dir();
        path.push(format!(
            "stratum-observer-config-test-{}-{}.toml",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("should be able to write a temporary config file");
        path
    }

    #[test]
    fn load_example_config() {
        let config = load_config_from_path(FILENAME_EXAMPLE_CONFIG).expect(&format!(
            "We should be able to load the {} file.",
            FILENAME_EXAMPLE_CONFIG
        ));
        assert_eq!(
            config.postgresql_url,
            Some(String::from(
                "postgres://<user>:<password>@<host>:<port>/<dbname>"
            ))
        );
        assert_eq!(
            config.websocket_address,
            Some(String::from("127.0.0.1:57127"))
        );
        assert_eq!(config.pools[0].endpoint, "stratum.example.com:3333");
        assert_eq!(config.pools[0].name, "Example Pool");
        assert_eq!(config.pools[0].user, "user.worker");
        assert_eq!(config.pools[0].password, Some(String::from("45324")));
    }

    /// This is the only test that touches the CONFIG_FILE environment
    /// variable. Keep it that way, otherwise the tests race with each other.
    #[test]
    fn load_config_from_envvar_path() {
        let path = write_config_file(
            r#"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "From the envvar", user = "username" },
            ]
        "#,
        );
        env::set_var(ENVVAR_CONFIG_FILE, &path);
        let config = load_config().expect("we should be able to load the config file");
        assert_eq!(config.pools[0].name, "From the envvar");
    }

    #[test]
    fn load_config_from_a_file() {
        let path = write_config_file(
            r#"
            postgresql_url = "postgres://user:password@host:1234/dbname"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "From a file", user = "username" },
            ]
        "#,
        );
        let config = load_config_from_path(&path.to_string_lossy())
            .expect("we should be able to load the config file");
        assert_eq!(
            config.postgresql_url,
            Some(String::from("postgres://user:password@host:1234/dbname"))
        );
        assert_eq!(config.pools[0].name, "From a file");
    }

    #[test]
    fn empty_database_url_and_websocket_address_disable_them() {
        let config = Config::from_toml_str(
            r#"
            postgresql_url = ""
            websocket_address = "   "
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username" },
            ]
        "#,
        )
        .expect("an empty url or address should not be an error");
        assert_eq!(config.postgresql_url, None);
        assert_eq!(config.websocket_address, None);
    }

    #[test]
    fn a_database_url_and_websocket_address_are_kept() {
        let config = Config::from_toml_str(
            r#"
            postgresql_url = "postgres://user:password@host:1234/dbname"
            websocket_address = "127.0.0.1:57127"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username" },
            ]
        "#,
        )
        .expect("we should be able to parse this config");
        assert_eq!(
            config.postgresql_url,
            Some(String::from("postgres://user:password@host:1234/dbname"))
        );
        assert_eq!(
            config.websocket_address,
            Some(String::from("127.0.0.1:57127"))
        );
    }

    #[test]
    fn load_pool_with_max_lifetime_config() {
        let config_string = r#"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username", password = "password123", max_lifetime=1337 },
                { endpoint = "stratum2.example.com:5674", name = "Example2", user = "username2" },
            ]
        "#;
        let config = Config::from_toml_str(config_string).unwrap();
        assert_eq!(config.pools[0].endpoint, "stratum.example.com:1234");
        assert_eq!(config.pools[0].name, "Example");
        assert_eq!(config.pools[0].user, "username");
        assert_eq!(config.pools[0].password, Some(String::from("password123")));
        assert_eq!(config.pools[0].max_lifetime, Some(1337));
        assert_eq!(config.pools[1].endpoint, "stratum2.example.com:5674");
        assert_eq!(config.pools[1].name, "Example2");
        assert_eq!(config.pools[1].user, "username2");
        assert_eq!(config.pools[1].password, None);
        assert_eq!(config.pools[1].max_lifetime, None);
    }

    /// Older config files may still contain the database_path setting from
    /// the time before PostgreSQL. They should keep working.
    #[test]
    fn settings_we_do_not_know_are_ignored() {
        let config = Config::from_toml_str(
            r#"
            database_path = "jobs.sqlite"
            websocket_address = "127.0.0.1:57127"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username" },
            ]
        "#,
        )
        .expect("a setting we do not know should not be an error");
        assert_eq!(
            config.websocket_address,
            Some(String::from("127.0.0.1:57127"))
        );
        assert_eq!(config.pools[0].name, "Example");
    }

    #[test]
    fn two_pools_with_the_same_name_are_rejected() {
        let error = Config::from_toml_str(
            r#"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username" },
                { endpoint = "stratum2.example.com:5674", name = "Example", user = "username2" },
            ]
        "#,
        )
        .expect_err("two pools with the same name should be an error");
        assert!(matches!(error, ConfigError::DuplicatePoolName(ref name) if name == "Example"));
    }

    #[test]
    fn pools_with_different_names_are_accepted() {
        let config = Config::from_toml_str(
            r#"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "Example", user = "username" },
                { endpoint = "stratum2.example.com:5674", name = "Example2", user = "username2" },
            ]
        "#,
        )
        .expect("two pools with different names should be fine");
        assert_eq!(config.pools.len(), 2);
    }

    #[test]
    fn a_config_without_pools_is_rejected() {
        let error = Config::from_toml_str(
            r#"
            websocket_address = "127.0.0.1:57127"
            pools = []
        "#,
        )
        .expect_err("a config without pools should be an error");
        assert!(matches!(error, ConfigError::NoPools));
    }

    #[test]
    fn a_pool_without_a_name_is_rejected() {
        let error = Config::from_toml_str(
            r#"
            pools = [
                { endpoint = "stratum.example.com:1234", name = "  ", user = "username" },
            ]
        "#,
        )
        .expect_err("a pool without a name should be an error");
        assert!(matches!(error, ConfigError::MissingPoolName));
    }

    #[test]
    fn a_pool_without_an_endpoint_is_rejected() {
        let error = Config::from_toml_str(
            r#"
            pools = [
                { endpoint = "", name = "Example", user = "username" },
            ]
        "#,
        )
        .expect_err("a pool without an endpoint should be an error");
        assert!(matches!(error, ConfigError::MissingPoolEndpoint(ref name) if name == "Example"));
    }
}
