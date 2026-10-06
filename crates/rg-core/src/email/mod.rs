//! Email notification service — send notification emails via SMTP.
//!
//! Configuration is provided via CLI flags or the `[smtp]` config section:
//! - `--smtp-host` — SMTP server hostname
//! - `--smtp-port` — SMTP server port (default 587)
//! - `--smtp-user` — SMTP username
//! - `--smtp-pass` — SMTP password
//! - `--smtp-from` — From email address
//!
//! With none of host/user/pass/from set, mail is off and sending is skipped.
//! `serve` refuses to start on a partial set rather than turning mail off.

use anyhow::Result;
use lettre::{
    message::header::ContentType, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde::{Deserialize, Serialize};

/// SMTP configuration for sending emails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub from: String,
}

impl SmtpConfig {
    /// Create a new SMTP config.
    pub fn new(host: &str, port: u16, user: &str, pass: &str, from: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            user: user.to_string(),
            pass: pass.to_string(),
            from: from.to_string(),
        }
    }

    /// Refuse a `from` no message could carry. It is otherwise parsed only at
    /// send time, so a typo there passes the start and then fails every
    /// notification and password-reset mail.
    pub fn validate_from(&self) -> Result<()> {
        self.from
            .parse::<lettre::message::Mailbox>()
            .map_err(|e| anyhow::anyhow!("invalid sender address {:?}: {e}", self.from))?;
        Ok(())
    }
}

/// Send a notification email.
pub async fn send_notification_email(
    config: &SmtpConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    let email = Message::builder()
        .from(config.from.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body.to_string())?;

    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?
        .port(config.port)
        .credentials(lettre::transport::smtp::authentication::Credentials::new(
            config.user.clone(),
            config.pass.clone(),
        ))
        .build();

    mailer.send(email).await?;
    Ok(())
}

/// Send a simple HTML notification email with a styled template.
pub async fn send_html_notification(
    config: &SmtpConfig,
    to: &str,
    title: &str,
    message: &str,
    action_url: Option<&str>,
) -> Result<()> {
    let action_html = match action_url {
        Some(url) => format!(
            r#"<div style="margin-top: 16px;"><a href="{}" style="background: #4f46e5; color: white; padding: 10px 20px; border-radius: 6px; text-decoration: none; display: inline-block;">View Details</a></div>"#,
            url
        ),
        None => String::new(),
    };

    let html_body = format!(
        r#"<html><body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #f6f8fa; margin: 0; padding: 20px;">
<div style="max-width: 600px; margin: 0 auto; background: white; border-radius: 8px; padding: 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.1);">
  <div style="border-bottom: 2px solid #4f46e5; padding-bottom: 12px; margin-bottom: 16px;">
    <h2 style="margin: 0; color: #1f2937;">{title}</h2>
  </div>
  <p style="color: #4b5563; line-height: 1.6;">{message}</p>
  {action_html}
  <hr style="border: none; border-top: 1px solid #e5e7eb; margin: 20px 0;" />
  <p style="color: #9ca3af; font-size: 12px;">You received this email because you have notifications enabled on Plombir Git.</p>
</div>
</body></html>"#,
        title = html_escape(title),
        message = html_escape(message),
        action_html = action_html,
    );

    send_notification_email(config, to, title, &html_body).await
}

/// Simple HTML entity escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
