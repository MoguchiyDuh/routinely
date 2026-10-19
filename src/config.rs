use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("environment variable {var} is not set")]
    Missing { var: &'static str },
    #[error("environment variable {var} has an invalid value")]
    InvalidValue {
        var: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub valkey_url: String,
    pub valkey_prefix: String,
    pub bind: SocketAddr,
    pub signup_code: String,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = required("ROUTINELY_DATABASE_URL")?;
        let valkey_url = required("ROUTINELY_VALKEY_URL")?;
        let valkey_prefix = match env::var("ROUTINELY_VALKEY_PREFIX") {
            Ok(value) => value,
            Err(_) => "routinely:".to_string(),
        };
        let bind = match env::var("ROUTINELY_BIND") {
            Ok(value) => match value.parse::<SocketAddr>() {
                Ok(addr) => addr,
                Err(source) => {
                    return Err(ConfigError::InvalidValue {
                        var: "ROUTINELY_BIND",
                        source: Box::new(source),
                    });
                }
            },
            Err(_) => SocketAddr::from(([127, 0, 0, 1], 8080)),
        };
        let signup_code = required("ROUTINELY_SIGNUP_CODE")?;
        let session_ttl = match env::var("ROUTINELY_SESSION_TTL_SECS") {
            Ok(value) => match value.parse::<u64>() {
                Ok(secs) => Duration::from_secs(secs),
                Err(source) => {
                    return Err(ConfigError::InvalidValue {
                        var: "ROUTINELY_SESSION_TTL_SECS",
                        source: Box::new(source),
                    });
                }
            },
            Err(_) => Duration::from_secs(2_592_000),
        };
        let cookie_secure = match env::var("ROUTINELY_COOKIE_SECURE") {
            Ok(value) => match value.parse::<bool>() {
                Ok(flag) => flag,
                Err(source) => {
                    return Err(ConfigError::InvalidValue {
                        var: "ROUTINELY_COOKIE_SECURE",
                        source: Box::new(source),
                    });
                }
            },
            Err(_) => false,
        };
        return Ok(Self {
            database_url,
            valkey_url,
            valkey_prefix,
            bind,
            signup_code,
            session_ttl,
            cookie_secure,
        });
    }
}

fn required(var: &'static str) -> Result<String, ConfigError> {
    return env::var(var).map_err(|_| ConfigError::Missing { var });
}
