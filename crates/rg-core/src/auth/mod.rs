//! Authentication — password hashing, JWT, CI tokens, LDAP, SSO, OCI tokens.
pub mod access_token;
pub mod at_rest_key;
pub mod ci_oidc;
pub mod ci_token;
pub mod credential_context;
pub mod encrypted_columns;
pub mod encryption;
pub mod instance_key;
pub mod jwt;
pub mod key_check;
pub mod ldap;
pub mod lockout;
/// Lives in `rg-process`, which the external runner shares; see there.
pub use rg_process::log_mask;
pub mod oci_token;
pub mod password;
pub mod pat_scope;
pub mod rekey;
pub mod ssh_key;
pub mod sso;
pub mod totp;
pub mod webauthn;
