//! TOTP 2FA service — Time-based One-Time Password (RFC 6238).

use anyhow::{Context, Result};
use qrcode::QrCode;
use std::time::SystemTime;
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Secret, TOTP};

/// Generate a new TOTP secret. Returns (secret_string, otpauth_url, qr_text).
///
/// The returned secret is **base32** (RFC 4648, unpadded) — the same encoding
/// the `secret=` parameter of the otpauth URL carries, so it can be typed into
/// an authenticator app by hand and fed straight back to [`verify_code`].
/// `Secret::Raw` renders as *hex* through `Display`, which is neither what the
/// app expects nor what `verify_code` parses — hence the explicit `to_encoded`.
pub fn generate_secret(username: &str, issuer: &str) -> Result<(String, String, String)> {
    // Secret::generate_secret() returns Secret directly in totp-rs v5
    let secret = Secret::generate_secret();
    let secret_bytes = secret
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("TOTP secret encoding error: {}", e))?;

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes,
        Some(issuer.to_string()),
        username.to_string(),
    )
    .map_err(|e| anyhow::anyhow!("TOTP config error: {}", e))?;

    let url = totp.get_url();
    let secret_str = secret.to_encoded().to_string();

    let qr = QrCode::new(&url).context("QR code generation failed")?;
    let qr_text = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .build();

    Ok((secret_str, url, qr_text))
}

/// Verify a TOTP code against a secret produced by [`generate_secret`].
///
/// `secret_str` is base32 (RFC 4648, unpadded). An unparseable secret is an
/// error, not a `false`: decoding it to an empty key would silently verify
/// every code against the wrong HMAC key and report "invalid code" for a
/// storage problem the operator needs to see. A clock before the Unix epoch is
/// likewise an error, not an invalid code.
///
/// A plain `bool` is the right answer only where the check is *not* a pass of
/// the second factor — enrolment, where the code proves the authenticator holds
/// the secret the server just handed it. Anything that hands out a session must
/// use [`verify_code_step`] and spend the step it returns, or the same code
/// passes again for as long as it stays inside the skew window.
pub fn verify_code(secret_str: &str, code: &str) -> Result<bool> {
    Ok(verify_code_step(secret_str, code)?.is_some())
}

/// Verify a TOTP code and report **which time step** it was derived from.
///
/// The step is what makes a successful check consumable: `TOTP::check` swallows
/// it, so a caller holding only its `bool` has nothing to record and no way to
/// tell a first use from a replay. RFC 6238 §5.2 — "the verifier MUST NOT
/// accept the second attempt of the OTP after the successful validation has
/// been issued for the first OTP" — is a statement about state the verifier
/// keeps, and there is no state to keep without this number.
///
/// `Ok(None)` is a code that matches no step in the window: an ordinary wrong
/// code, not a failure.
pub fn verify_code_step(secret_str: &str, code: &str) -> Result<Option<u64>> {
    let secret_bytes = Secret::Encoded(secret_str.to_string())
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("TOTP secret is not valid base32: {}", e))?;

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes,
        None,
        "".to_string(),
    )
    .map_err(|e| anyhow::anyhow!("TOTP parse error: {}", e))?;

    matching_step_at(&totp, code, SystemTime::now())
}

/// The step `code` belongs to, searched over the same window `TOTP::check` uses.
///
/// Deliberately a re-implementation of `check` rather than a call to it: the
/// crate's version returns a `bool` and drops the step. The window is
/// `[now/step - skew, now/step + skew]`, and each candidate is compared in
/// constant time so a wrong code leaks nothing about how far along the window
/// it failed.
///
/// At most one step can match a given code — each step derives a different HMAC
/// — so "the first match wins" is not a policy choice, it is the only match.
fn matching_step_at(totp: &TOTP, code: &str, now: SystemTime) -> Result<Option<u64>> {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("totp: read the current time step")?
        .as_secs();

    let step_secs = totp.step;
    let skew = u64::from(totp.skew);
    // `saturating_sub` rather than `-`: a clock inside the first skew window of
    // the epoch is a test fixture, not a reason to panic on an underflow.
    let first = (seconds / step_secs).saturating_sub(skew);

    for step in first..=(seconds / step_secs + skew) {
        let candidate = totp.generate(step * step_secs);
        // `ct_eq` and not `==`: `str`'s comparison exits on the first differing
        // byte, which is what the crate's own `check` avoids and what this
        // re-implementation must keep avoiding. Length is compared openly first —
        // a code of the wrong length is not a secret — but `ct_eq` needs equal
        // slices anyway.
        if candidate.len() == code.len() && bool::from(candidate.as_bytes().ct_eq(code.as_bytes()))
        {
            return Ok(Some(step));
        }
    }
    Ok(None)
}

/// Generate QR code as SVG for web display.
pub fn generate_qr_svg(otpauth_url: &str) -> String {
    let qr = match QrCode::new(otpauth_url) {
        Ok(qr) => qr,
        Err(_) => return String::new(),
    };

    let size = 200;
    let module_count = qr.width() as f64;
    let padding = 4.0f64;
    let total_modules = module_count + 2.0 * padding;
    let scale = (size as f64) / total_modules;
    let offset = scale * padding;

    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">"#,
        size = size
    );
    svg.push_str(&format!(
        r#"<rect width="{size}" height="{size}" fill="white"/>"#,
        size = size
    ));

    // qrcode v0.14: to_colors() returns Vec<Color>
    let colors = qr.to_colors();
    let width = qr.width();
    for (idx, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Light {
            let x = idx % width;
            let y = idx / width;
            let rx = (offset + x as f64 * scale).ceil();
            let ry = (offset + y as f64 * scale).ceil();
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="black"/>"#,
                rx,
                ry,
                scale.ceil(),
                scale.ceil()
            ));
        }
    }

    svg.push_str("</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_generate_secret() {
        let (secret, _url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();
        assert!(!secret.is_empty());
    }

    /// The whole point of enrollment: a code produced from the secret we handed
    /// to the authenticator app must verify against the secret we stored.
    #[test]
    fn code_from_enrollment_url_verifies_against_stored_secret() {
        let (stored_secret, url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();

        // What the authenticator app does: parse the otpauth:// URL it scanned
        // and generate the current code from it.
        let app_totp = TOTP::from_url(&url).expect("otpauth URL must be parseable");
        let code = app_totp.generate_current().expect("code generation");

        assert!(
            verify_code(&stored_secret, &code).unwrap(),
            "stored secret must accept the code the enrolled app generates"
        );

        let wrong_code = format!(
            "{}{}",
            &code[..code.len() - 1],
            if code.ends_with('0') { '1' } else { '0' }
        );
        assert!(
            !verify_code(&stored_secret, &wrong_code).unwrap(),
            "a checked but incorrect code must stay a plain rejection"
        );
    }

    /// The secret we show for manual entry must be the one in the QR code.
    #[test]
    fn returned_secret_matches_the_otpauth_url_parameter() {
        let (stored_secret, url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();
        let url_secret = url
            .split("secret=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("otpauth URL carries a secret parameter");
        assert_eq!(stored_secret, url_secret);
    }

    /// A secret that cannot be decoded is a storage fault, not a wrong code.
    #[test]
    fn unparseable_secret_is_an_error_not_a_silent_false() {
        assert!(verify_code("not base32 at all!!", "123456").is_err());
    }

    #[test]
    fn a_clock_before_the_epoch_is_an_error_not_a_wrong_code() {
        let (secret, url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();
        let totp = TOTP::from_url(&url).expect("otpauth URL must be parseable");
        let clock_before_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(1);

        let error = matching_step_at(&totp, "123456", clock_before_epoch).unwrap_err();

        assert!(error
            .to_string()
            .contains("totp: read the current time step"));
        assert!(verify_code(&secret, "123456").is_ok());
    }

    /// The step is the whole point of `verify_code_step`: a caller that only
    /// learns "valid" has nothing to spend, which is how one code passed the
    /// second factor for its whole 90-second window (card_9585caf5692d).
    #[test]
    fn a_valid_code_reports_the_step_it_was_derived_from() {
        let (stored_secret, url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();
        let app_totp = TOTP::from_url(&url).expect("otpauth URL must be parseable");

        // Fixed instant rather than `now()`: the assertion is about which step
        // is reported, and a test that reads the clock twice can straddle a
        // boundary between the two reads.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_777_777_777);
        let expected_step = 1_777_777_777 / 30;
        let code = app_totp.generate(expected_step * 30);

        let server_totp = TOTP::from_url(&url).expect("otpauth URL must be parseable");
        assert_eq!(
            matching_step_at(&server_totp, &code, now).unwrap(),
            Some(expected_step),
            "a valid code must name its own step"
        );

        // Non-vacuity: the same helper still refuses a code from no step at all.
        assert_eq!(matching_step_at(&server_totp, "000000", now).unwrap(), None);
        assert!(verify_code_step(&stored_secret, "000000").is_ok());
    }

    /// Clock skew is still tolerated, and each neighbour reports *its own* step
    /// — otherwise spending a step would either reject an honest neighbour or
    /// mark the wrong one as spent.
    #[test]
    fn each_step_in_the_skew_window_reports_itself() {
        let (_secret, url, _qr) = generate_secret("testuser", "ForgeKeep").unwrap();
        let totp = TOTP::from_url(&url).expect("otpauth URL must be parseable");

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_777_777_777);
        let current = 1_777_777_777 / 30;

        for step in [current - 1, current, current + 1] {
            let code = totp.generate(step * 30);
            assert_eq!(
                matching_step_at(&totp, &code, now).unwrap(),
                Some(step),
                "the code for step {step} must report step {step}"
            );
        }

        // Just outside the window, in both directions.
        for step in [current - 2, current + 2] {
            let code = totp.generate(step * 30);
            assert_eq!(
                matching_step_at(&totp, &code, now).unwrap(),
                None,
                "step {step} is outside the skew window and must not verify"
            );
        }
    }
}
