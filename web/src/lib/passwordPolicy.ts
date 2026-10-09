/**
 * The password rules a new password has to meet — the server's
 * `PasswordValidator::standard()` (`crates/rg-core/src/auth/password.rs`),
 * restated so a form can say them before it is submitted rather than after a
 * refusal (card_e1baa94866ed). `passwordPolicy.test.ts` reads the Rust
 * defaults, so the two cannot drift apart silently.
 *
 * The browser enforces only the lengths; the character classes, the common
 * password list and "not your username" stay with the server, which names the
 * rule that failed.
 */
export const PASSWORD_MIN_LENGTH = 8;
export const PASSWORD_MAX_LENGTH = 128;
