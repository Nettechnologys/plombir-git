//! The one digest every bearer credential is stored and looked up under.
//!
//! Issuance and verification must call this same function. A token minted under
//! one spelling and checked under another is a credential that can never be
//! used, or a check that accepts an input nothing ever issued.
//!
//! The digest is plain SHA-256 with no salt, because the lookup is *by* the
//! digest and there is nowhere to put a per-row salt. That is sound because
//! every token is generated from the CSPRNG and carries at least 122 bits of
//! entropy, which no dictionary or rainbow table reaches. The input is never
//! user-chosen, so the function does not normalise it: `tok` and ` tok` are
//! different credentials.
//!
//! The output is lower-case hex, byte-identical to the `hex::encode` spelling
//! that earlier rows were written with, so no stored digest changes.

use sha2::{Digest, Sha256};

/// Lower-case hex SHA-256 of `token`, as stored in every `token_hash` column.
pub fn token_digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::token_digest;

    #[test]
    fn matches_the_published_sha256_vector() {
        assert_eq!(
            token_digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn does_not_normalise_its_input() {
        assert_ne!(token_digest("tok"), token_digest(" tok"));
        assert_ne!(token_digest("tok"), token_digest("tok\n"));
    }
}
