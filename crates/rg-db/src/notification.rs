//! Shared notification vocabulary and bounded, non-credential event metadata.
use serde::{Deserialize, Serialize};

macro_rules! events {
    ($($variant:ident => $key:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Event { $($variant),+ }
        impl Event {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn key(self) -> &'static str { match self { $(Self::$variant => $key),+ } }
            pub fn parse(value: &str) -> Option<Self> { match value { $($key => Some(Self::$variant),)+ _ => None } }
        }
    }
}

events! {
    ReviewRequested => "review_requested", Mention => "mention", Assigned => "assigned",
    CiFailed => "ci_failed", Participating => "participating", CiTriggered => "ci_triggered",
    NewDevice => "new_device", TokenCreated => "token_created", TokenRevoked => "token_revoked",
    AgentTokenCreated => "agent_token_created", AgentTokenRevoked => "agent_token_revoked",
    SshKeyAdded => "ssh_key_added", SshKeyRemoved => "ssh_key_removed",
    SigningKeyAdded => "signing_key_added", SigningKeyRemoved => "signing_key_removed",
    DeployKeyAdded => "deploy_key_added", DeployKeyRemoved => "deploy_key_removed",
    PasskeyAdded => "passkey_added", PasskeyRemoved => "passkey_removed",
    MfaEnabled => "mfa_enabled", MfaReplaced => "mfa_replaced", MfaDisabled => "mfa_disabled",
    BackupCodesRegenerated => "backup_codes_regenerated", PasswordChanged => "password_changed",
    PasswordReset => "password_reset", EmailChanged => "email_changed",
    ExternalLoginLinked => "external_login_linked", ExternalLoginUnlinked => "external_login_unlinked",
    AdminPasswordReset => "admin_password_reset", AdminFactorsReset => "admin_factors_reset",
    RunnerTokenCreated => "runner_token_created", RunnerTokenRevoked => "runner_token_revoked",
    NotificationSettingsChanged => "notification_settings_changed",
    AgentProfileChanged => "agent_profile_changed", BotCreated => "bot_created", BotDeleted => "bot_deleted",
}

impl Event {
    pub fn security(self) -> bool {
        !matches!(
            self,
            Self::ReviewRequested
                | Self::Mention
                | Self::Assigned
                | Self::CiFailed
                | Self::Participating
                | Self::CiTriggered
        )
    }

    pub fn default_email(self) -> bool {
        self != Self::CiTriggered
    }

    pub fn legacy_email(
        self,
        settings: &crate::entities::notification_setting::Model,
    ) -> Option<bool> {
        Some(match self {
            Self::ReviewRequested => settings.email_review_requested,
            Self::Mention => settings.email_mention,
            Self::Assigned => settings.email_assigned,
            Self::CiFailed => settings.email_ci_failed,
            Self::Participating => settings.email_participating,
            Self::CiTriggered => settings.email_ci_triggered,
            _ => return None,
        })
    }

    pub fn set_legacy_email(
        self,
        settings: &mut crate::entities::notification_setting::Model,
        value: bool,
    ) {
        match self {
            Self::ReviewRequested => settings.email_review_requested = value,
            Self::Mention => settings.email_mention = value,
            Self::Assigned => settings.email_assigned = value,
            Self::CiFailed => settings.email_ci_failed = value,
            Self::Participating => settings.email_participating = value,
            Self::CiTriggered => settings.email_ci_triggered = value,
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ru")]
    Ru,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Locale {
    pub fn key(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
            Self::ZhCn => "zh-CN",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Self::En),
            "ru" => Some(Self::Ru),
            "zh-CN" => Some(Self::ZhCn),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub email: bool,
    pub telegram: bool,
}

/// The only payload accepted by the durable queue. Deliberately no arbitrary
/// details/body map: audit metadata may include fields inappropriate for mail.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub actor: Option<String>,
    pub name: Option<String>,
    pub fingerprint: Option<String>,
    pub ip: Option<String>,
    pub device: Option<String>,
    pub method: Option<String>,
    pub subject: Option<String>,
    pub link: Option<String>,
    pub changed: Vec<String>,
}

impl Metadata {
    pub fn bounded(mut self) -> Self {
        for text in [
            &mut self.actor,
            &mut self.name,
            &mut self.fingerprint,
            &mut self.device,
            &mut self.method,
            &mut self.subject,
        ]
        .into_iter()
        .flatten()
        {
            *text = text.chars().filter(|c| !c.is_control()).take(256).collect();
        }
        self.ip = self
            .ip
            .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string());
        self.link = self.link.filter(|link| {
            link.starts_with('/')
                && !link.starts_with("//")
                && !link.contains(['\\', '\r', '\n'])
                && link.len() <= 512
        });
        self.changed = self
            .changed
            .into_iter()
            .take(80)
            .map(|value| {
                value
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
                    .take(80)
                    .collect()
            })
            .collect();
        self
    }
}

// Request context is safe metadata only; transport tokens/passwords never
// enter it. Database producers also work without HTTP (CLI/system actions).
tokio::task_local! { static CONTEXT: (Option<i64>, Metadata); }

pub async fn scope<F: std::future::Future>(
    actor_id: Option<i64>,
    metadata: Metadata,
    future: F,
) -> F::Output {
    CONTEXT.scope((actor_id, metadata.bounded()), future).await
}

pub fn current_metadata() -> Metadata {
    CONTEXT
        .try_with(|context| context.1.clone())
        .unwrap_or_default()
}

pub fn current_actor_id() -> Option<i64> {
    CONTEXT.try_with(|context| context.0).ok().flatten()
}
