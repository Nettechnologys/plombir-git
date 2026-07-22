//! Authentication — password hashing, JWT, CI tokens, LDAP, SSO, OCI tokens.
pub mod ci_oidc;
pub mod ci_token;
pub mod encryption;
pub mod jwt;
pub mod ldap;
pub mod oci_token;
pub mod password;
pub mod pat_scope;
pub mod ssh_key;
pub mod sso;
pub mod totp;
