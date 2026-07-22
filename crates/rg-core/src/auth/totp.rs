//! TOTP 2FA service — Time-based One-Time Password (RFC 6238).

use anyhow::{Context, Result};
use qrcode::QrCode;
use totp_rs::{Algorithm, Secret, TOTP};

/// Generate a new TOTP secret. Returns (secret_string, otpauth_url, qr_text).
pub fn generate_secret(username: &str, issuer: &str) -> Result<(String, String, String)> {
    // Secret::generate_secret() returns Secret directly in totp-rs v5
    let secret = Secret::generate_secret();

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret.to_bytes().unwrap_or_default(),
        Some(issuer.to_string()),
        username.to_string(),
    )
    .map_err(|e| anyhow::anyhow!("TOTP config error: {}", e))?;

    let url = totp.get_url();
    let secret_str = secret.to_string();

    let qr = QrCode::new(&url).context("QR code generation failed")?;
    let qr_text = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .build();

    Ok((secret_str, url, qr_text))
}

/// Verify a TOTP code for a given secret.
pub fn verify_code(secret_str: &str, code: &str) -> Result<bool> {
    let secret = Secret::Raw(secret_str.to_string().into_bytes());

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret.to_bytes().unwrap_or_default(),
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
        let (secret, _url, _qr) = generate_secret("testuser", "IronForge").unwrap();
        assert!(!secret.is_empty());
    }
}
