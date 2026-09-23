use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("environment variable ROUTINELY_SIGNUP_CODE is not set")]
    MissingSignupCode,
    #[error("environment variable {var} has an invalid value")]
    InvalidValue {
        var: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: PathBuf,
    pub bind: SocketAddr,
    pub signup_code: String,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let db_path = match env::var("ROUTINELY_DB_PATH") {
            Ok(value) => PathBuf::from(value),
            Err(_) => PathBuf::from("data/routinely.db"),
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
        let signup_code = match env::var("ROUTINELY_SIGNUP_CODE") {
            Ok(value) => value,
            Err(_) => return Err(ConfigError::MissingSignupCode),
        };
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
            db_path,
            bind,
            signup_code,
            session_ttl,
            cookie_secure,
        });
    }
}
