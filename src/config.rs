use serde::Deserialize;
use std::error;
use std::io;
use std::{env, fmt, fs};

pub const ENVVAR_CONFIG_FILE: &str = "CONFIG_FILE";
const DEFAULT_CONFIG: &str = "config.toml";

use crate::types::Pool;
use log::info;

#[derive(Deserialize)]
pub struct Config {
    pub database_path: String,
    pub pools: Vec<Pool>,
}

pub fn load_config() -> Result<Config, ConfigError> {
    let config_file_path =
        env::var(ENVVAR_CONFIG_FILE).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    info!("Reading configuration file from {}.", config_file_path);
    let config_string = fs::read_to_string(config_file_path)?;
    let config: Config = toml::from_str(&config_string)?;
    Ok(config)
}

#[derive(Debug)]
pub enum ConfigError {
    TomlError(toml::de::Error),
    ReadError(io::Error),
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
        }
    }
}

impl error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ConfigError::TomlError(ref e) => Some(e),
            ConfigError::ReadError(ref e) => Some(e),
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

    #[test]
    fn load_example_config() {
        use std::env;

        const FILENAME_EXAMPLE_CONFIG: &str = "config.toml.example";
        env::set_var(ENVVAR_CONFIG_FILE, FILENAME_EXAMPLE_CONFIG);
        let cfg = load_config().expect(&format!(
            "We should be able to load the {} file.",
            FILENAME_EXAMPLE_CONFIG
        ));
        // FIXME
        // assert ..
    }

    #[test]
    fn load_pool_with_max_lifetime_config() {
        let config_string = r#"
            database_path = "abc"
            pools = [
                { endpoint = "stratum.example.com", name = "Example", user = "username", password = "password123", max_lifetime=1337 },
                { endpoint = "stratum2.example.com", name = "Example2", user = "username2", password = "123password" },
            ]
        "#;
        let config: Config = toml::from_str(&config_string).unwrap();
        assert_eq!(config.database_path, "abc");
        assert_eq!(config.pools[0].endpoint, "stratum.example.com");
        assert_eq!(config.pools[0].name, "Example");
        assert_eq!(config.pools[0].user, "username");
        assert_eq!(config.pools[0].password, "password123");
        assert_eq!(config.pools[0].max_lifetime, Some(1337));
        assert_eq!(config.pools[1].endpoint, "stratum2.example.com");
        assert_eq!(config.pools[1].name, "Example2");
        assert_eq!(config.pools[1].user, "username2");
        assert_eq!(config.pools[1].password, "123password");
        assert_eq!(config.pools[1].max_lifetime, None);
    }
}
