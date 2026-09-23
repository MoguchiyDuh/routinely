use argon2::Algorithm;
use argon2::Argon2;
use argon2::Params;
use argon2::Version;
use argon2::password_hash::PasswordHasher;
use argon2::password_hash::PasswordVerifier;
use argon2::password_hash::phc::PasswordHash;
use thiserror::Error;

pub const MIN_PASSWORD_BYTES: usize = 8;
pub const MAX_PASSWORD_BYTES: usize = 128;

const ARGON2_M_COST_KIB: u32 = 19_456;
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("password length must be between 8 and 128 bytes")]
    Length,
    #[error("failed to hash password")]
    Hash,
}

pub fn hash(password: &str) -> Result<String, PasswordError> {
    let bytes = password.as_bytes();
    if bytes.len() < MIN_PASSWORD_BYTES || bytes.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::Length);
    }
    let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
        .map_err(|_| PasswordError::Hash)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let parsed = argon2
        .hash_password(bytes)
        .map_err(|_| PasswordError::Hash)?;
    return Ok(parsed.to_string());
}

pub fn verify(password: &str, stored: &str) -> Result<bool, PasswordError> {
    let bytes = password.as_bytes();
    if bytes.len() > MAX_PASSWORD_BYTES {
        return Ok(false);
    }
    let parsed = match PasswordHash::new(stored) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(false),
    };
    let argon2 = Argon2::default();
    return Ok(argon2.verify_password(bytes, &parsed).is_ok());
}
