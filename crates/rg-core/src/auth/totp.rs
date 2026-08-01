//! TOTP 2FA service — Time-based One-Time Password (RFC 6238).

use anyhow::{Context, Result};
use qrcode::QrCode;
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
/// storage problem the operator needs to see.
pub fn verify_code(secret_str: &str, code: &str) -> Result<bool> {
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

    Ok(totp.check_current(code).unwrap_or(false))
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
}
