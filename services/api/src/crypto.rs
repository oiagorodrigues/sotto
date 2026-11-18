use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("random source failed")]
    Random,
    #[error("password hashing failed")]
    Hash,
}

/// 32 random bytes as 43 base64url characters.
pub fn random_token() -> Result<String, CryptoError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|_| CryptoError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Tokens carry 256 bits of entropy, so a plain SHA-256 is enough to store them.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn pkce_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// OWASP minimum for Argon2id: 19 MiB, 2 iterations, 1 lane.
fn argon2() -> Result<Argon2<'static>, CryptoError> {
    let params = Params::new(19 * 1024, 2, 1, None).map_err(|_| CryptoError::Hash)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn hash_password(password: &str) -> Result<String, CryptoError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| CryptoError::Random)?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| CryptoError::Hash)?;
    argon2()?
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| CryptoError::Hash)
}

/// `false` for a wrong password and for a malformed stored hash.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    let Ok(argon) = argon2() else {
        return false;
    };
    argon.verify_password(password.as_bytes(), &parsed).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_43_chars_and_unique() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
    }

    #[test]
    fn pkce_matches_rfc7636_example() {
        assert_eq!(
            pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn password_round_trip() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
    }
}
