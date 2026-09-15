use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::random;

pub const SECRET_BYTES: usize = 32;

pub fn generate() -> String {
    URL_SAFE_NO_PAD.encode(random::bytes::<SECRET_BYTES>())
}

pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn verify(secret: &str, expected_hash: &str) -> bool {
    hash(secret).as_bytes().ct_eq(expected_hash.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_secret_is_thirty_two_bytes_of_url_safe_base64() {
        let secret = generate();
        let Ok(decoded) = URL_SAFE_NO_PAD.decode(&secret) else {
            panic!("{secret} does not decode as url-safe base64");
        };

        assert_eq!(decoded.len(), SECRET_BYTES, "{secret}");
        assert!(!secret.contains(['+', '/', '=']), "{secret} is url safe and unpadded");
    }

    #[test]
    fn two_generated_secrets_differ() {
        assert_ne!(generate(), generate(), "secrets come from the csprng, not a counter");
    }

    #[test]
    fn a_hash_is_sixty_four_hex_characters_of_sha256() {
        let cases = [
            ("the empty string", "", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            ("a known secret", "abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        ];
        for (label, secret, expected) in cases {
            assert_eq!(hash(secret), expected, "{label}");
        }
    }

    #[test]
    fn verify_accepts_only_the_secret_behind_the_stored_hash() {
        let secret = generate();
        let stored = hash(&secret);
        let mut wrong_same_length = secret.clone();
        wrong_same_length.replace_range(0..1, if secret.starts_with('a') { "b" } else { "a" });
        let mut wrong_different_length = secret.clone();
        wrong_different_length.pop();
        let mut stored_one_digit_off = stored.clone();
        stored_one_digit_off.replace_range(0..1, if stored.starts_with('a') { "b" } else { "a" });

        let cases = [
            ("the right secret", secret.clone(), stored.clone(), true),
            ("a wrong secret of the same length", wrong_same_length, stored.clone(), false),
            ("a wrong secret of a different length", wrong_different_length, stored.clone(), false),
            ("an empty secret checked against a real hash", String::new(), stored.clone(), false),
            ("an empty secret checked against its own hash", String::new(), hash(""), true),
            ("the stored hash presented as the secret", stored.clone(), stored.clone(), false),
            ("a stored hash one hex digit off", secret.clone(), stored_one_digit_off, false),
            ("an empty stored hash", secret.clone(), String::new(), false),
        ];
        for (label, candidate, expected_hash, expected) in cases {
            assert_eq!(verify(&candidate, &expected_hash), expected, "{label}");
        }
    }
}
