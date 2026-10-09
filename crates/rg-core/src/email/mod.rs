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
    let html_body = notification_html(title, message, action_url)?;
    send_notification_email(config, to, title, &html_body).await
}

/// The body [`send_html_notification`] mails.
///
/// `title` and `message` are text, never markup: callers put names a pushing
/// collaborator chose into them (a branch called `<a href=…>Confirm your
/// login</a>` is a valid ref), so both are escaped and a line break in the
/// message is the only formatting it gets. The action link is an `http(s)`
/// URL or nothing is sent — every caller builds it from the instance's own
/// address, so anything else is a bug to surface, not a link to mail.
fn notification_html(title: &str, message: &str, action_url: Option<&str>) -> Result<String> {
    let action_html = match action_url {
        Some(url) => {
            let href = http_link(url)
                .ok_or_else(|| anyhow::anyhow!("refusing to mail a non-http(s) action link"))?;
            format!(
                r#"<div style="margin-top: 16px;"><a href="{href}" style="background: #4f46e5; color: white; padding: 10px 20px; border-radius: 6px; text-decoration: none; display: inline-block;">View Details</a></div>"#
            )
        }
        None => String::new(),
    };

    Ok(format!(
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
        message = html_escape(message).replace('\n', "<br/>"),
        action_html = action_html,
    ))
}

/// `url` escaped for an `href`, when it is an `http(s)` URL; `None` for any
/// other scheme, so a `javascript:` or `data:` value never becomes a link.
fn http_link(url: &str) -> Option<String> {
    let url = url.trim();
    let scheme_ok = url
        .get(..8)
        .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
        || url
            .get(..7)
            .is_some_and(|head| head.eq_ignore_ascii_case("http://"));
    scheme_ok.then(|| html_escape(url))
}

/// Send one notification mail: one entry per notification that piled up for
/// the recipient, each with its link, and a footer that says where these mails
/// are turned off.
pub async fn send_digest(
    config: &SmtpConfig,
    to: &str,
    mail: &crate::notification::mail::Digest,
) -> Result<()> {
    let entries: String = mail
        .entries
        .iter()
        .map(|entry| {
            let title = match entry.url.as_deref().and_then(http_link) {
                Some(href) => format!(
                    r#"<a href="{href}" style="color: #4f46e5; text-decoration: none;">{}</a>"#,
                    html_escape(&entry.title)
                ),
                None => html_escape(&entry.title),
            };
            let body = entry
                .body
                .as_deref()
                .map(|body| {
                    format!(
                        r#"<div style="color: #4b5563;">{}</div>"#,
                        html_escape(body)
                    )
                })
                .unwrap_or_default();
            format!(r#"<li style="margin-bottom: 12px;"><strong>{title}</strong>{body}</li>"#)
        })
        .collect();
    let footer = match mail.settings_url.as_deref().and_then(http_link) {
        Some(href) => format!(
            r#"You received this email because of your notification settings: <a href="{href}" style="color: #9ca3af;">change what is mailed to you</a>."#
        ),
        None => "You received this email because of your notification settings on Plombir Git \
                 (Settings → Notifications)."
            .to_string(),
    };
    let html_body = format!(
        r#"<html><body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #f6f8fa; margin: 0; padding: 20px;">
<div style="max-width: 600px; margin: 0 auto; background: white; border-radius: 8px; padding: 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.1);">
  <ul style="padding-left: 18px; margin: 0;">{entries}</ul>
  <hr style="border: none; border-top: 1px solid #e5e7eb; margin: 20px 0;" />
  <p style="color: #9ca3af; font-size: 12px;">{footer}</p>
</div>
</body></html>"#
    );
    send_notification_email(config, to, &mail.subject, &html_body).await
}

/// Simple HTML entity escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{http_link, notification_html};

    #[test]
    fn a_branch_name_made_of_markup_reaches_the_mail_as_text() {
        let html = notification_html(
            "CI pipeline triggered",
            "on branch refs/heads/<a href=\"https://evil.example\">Confirm your login</a>",
            None,
        )
        .expect("render");
        assert!(
            !html.contains(r#"<a href="https://evil.example""#),
            "{html}"
        );
        assert!(
            html.contains("&lt;a href=&quot;https://evil.example&quot;&gt;"),
            "{html}"
        );
    }

    #[test]
    fn line_breaks_in_the_message_are_the_only_markup_it_gets() {
        let html = notification_html("t", "first\n\nsecond", None).expect("render");
        assert!(html.contains("first<br/><br/>second"), "{html}");
        assert!(!html.contains("&lt;br/&gt;"), "{html}");
    }

    #[test]
    fn only_an_http_link_becomes_a_button() {
        let html = notification_html("t", "m", Some("https://git.example/reset?token=a\"b"))
            .expect("render");
        assert!(
            html.contains(r#"href="https://git.example/reset?token=a&quot;b""#),
            "{html}"
        );
        for hostile in ["javascript:alert(1)", "data:text/html,x", " JaVaScRiPt:x"] {
            assert!(
                notification_html("t", "m", Some(hostile)).is_err(),
                "{hostile}"
            );
            assert!(http_link(hostile).is_none(), "{hostile}");
        }
        assert!(http_link("HTTP://git.example/").is_some());
    }
}
