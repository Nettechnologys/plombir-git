//! Personal delivery settings: one legacy-compatible writer, authorized on
//! its locked account snapshot through the existing M2 current-factor guard.
use crate::{
    api::auth::{require_sudo_for_user, LoginSession},
    error::AppError,
    AppState,
};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use rg_core::notification::settings;
use rg_db::{
    notification::{Event, Locale, Metadata},
    ops::notification_delivery_ops as queue,
};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmailNotificationSettings {
    pub review_requested: bool,
    pub mention: bool,
    pub assigned: bool,
    pub ci_failed: bool,
    pub participating: bool,
    pub ci_triggered: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NotificationChannels {
    pub email: bool,
    pub telegram: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TelegramNotificationSettings {
    pub token_configured: bool,
    pub chat_id: Option<i64>,
    pub message_thread_id: Option<i32>,
    pub confirmed: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NotificationCapabilities {
    pub email: bool,
    pub email_reason: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NotificationSettingsResponse {
    pub email: EmailNotificationSettings,
    pub events: BTreeMap<String, NotificationChannels>,
    pub locale: String,
    pub revision: i64,
    pub telegram: TelegramNotificationSettings,
    pub capabilities: NotificationCapabilities,
}

#[derive(Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailNotificationSettingsPatch {
    pub review_requested: Option<bool>,
    pub mention: Option<bool>,
    pub assigned: Option<bool>,
    pub ci_failed: Option<bool>,
    pub participating: Option<bool>,
    pub ci_triggered: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationChannelPatch {
    pub email: Option<bool>,
    pub telegram: Option<bool>,
}

/// Input only. Never derive Debug/Serialize for a credential-bearing request.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TelegramNotificationPatch {
    Keep,
    Set {
        token: Option<String>,
        chat_id: i64,
        message_thread_id: Option<i32>,
    },
    Remove,
}

#[derive(Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateNotificationSettingsRequest {
    pub email: Option<EmailNotificationSettingsPatch>,
    #[serde(default)]
    pub events: BTreeMap<String, NotificationChannelPatch>,
    pub locale: Option<String>,
    pub telegram: Option<TelegramNotificationPatch>,
}

impl UpdateNotificationSettingsRequest {
    fn patch(self) -> Result<settings::Patch, AppError> {
        let email = self.email.unwrap_or_default();
        let mut events = BTreeMap::new();
        for (key, patch) in self.events {
            let event = Event::parse(&key)
                .ok_or_else(|| AppError::bad_request("unknown notification event"))?;
            events.insert(
                event,
                settings::ChannelPatch {
                    email: patch.email,
                    telegram: patch.telegram,
                },
            );
        }
        Ok(settings::Patch {
            email: [
                (Event::ReviewRequested, email.review_requested),
                (Event::Mention, email.mention),
                (Event::Assigned, email.assigned),
                (Event::CiFailed, email.ci_failed),
                (Event::Participating, email.participating),
                (Event::CiTriggered, email.ci_triggered),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect(),
            events,
            locale: self
                .locale
                .map(|locale| {
                    Locale::parse(&locale)
                        .ok_or_else(|| AppError::bad_request("unsupported notification language"))
                })
                .transpose()?,
            telegram: self.telegram.map(|change| match change {
                TelegramNotificationPatch::Keep => settings::TelegramChange::Keep,
                TelegramNotificationPatch::Set {
                    token,
                    chat_id,
                    message_thread_id,
                } => settings::TelegramChange::Set {
                    token,
                    chat_id,
                    message_thread_id,
                },
                TelegramNotificationPatch::Remove => settings::TelegramChange::Remove,
            }),
        })
    }
}

fn response(settings: settings::Settings, smtp: bool) -> NotificationSettingsResponse {
    let legacy = settings.legacy;
    NotificationSettingsResponse {
        email: EmailNotificationSettings {
            review_requested: legacy.email_review_requested,
            mention: legacy.email_mention,
            assigned: legacy.email_assigned,
            ci_failed: legacy.email_ci_failed,
            participating: legacy.email_participating,
            ci_triggered: legacy.email_ci_triggered,
        },
        events: settings
            .events
            .into_iter()
            .map(|(key, value)| {
                (
                    key.key().into(),
                    NotificationChannels {
                        email: value.email,
                        telegram: value.telegram,
                    },
                )
            })
            .collect(),
        locale: settings.profile.locale,
        revision: settings.profile.revision,
        telegram: TelegramNotificationSettings {
            token_configured: settings.profile.telegram_token_encrypted.is_some(),
            chat_id: settings.profile.chat_id,
            message_thread_id: settings.profile.message_thread_id,
            confirmed: settings.profile.telegram_confirmed,
        },
        capabilities: NotificationCapabilities {
            email: smtp,
            email_reason: (!smtp).then(|| "smtp_unavailable".into()),
        },
    }
}

fn error_response(error: anyhow::Error) -> Response {
    // Preserve the existing guard's 401 session_revoked / 403 sudo_required;
    // AppError::from(anyhow) otherwise erases this distinction.
    match error.downcast::<AppError>() {
        Ok(error) => error.into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(get,path="/users/me/notification-settings",tag="Notifications",responses(
    (status=200,description="Saved personal channels and language; no credential",body=NotificationSettingsResponse),
    (status=401,description="Login session required",body=serde_json::Value),
))]
pub async fn get_notification_settings(
    State(state): State<AppState>,
    LoginSession { user_id, .. }: LoginSession,
) -> Response {
    match settings::get(&state.db_write, user_id).await {
        Ok(settings) => Json(response(settings, state.smtp_config.is_some())).into_response(),
        Err(error) => error_response(error),
    }
}

#[utoipa::path(put,path="/users/me/notification-settings",tag="Notifications",request_body=UpdateNotificationSettingsRequest,responses(
    (status=200,description="Atomic patch saved",body=NotificationSettingsResponse),
    (status=400,description="Invalid settings",body=serde_json::Value),
    (status=401,description="Login session required or revoked",body=serde_json::Value),
    (status=403,description="Current-factor fresh proof required for reduced delivery or Telegram changes",body=serde_json::Value),
))]
pub async fn update_notification_settings(
    State(state): State<AppState>,
    LoginSession { user_id, claims }: LoginSession,
    headers: HeaderMap,
    Json(request): Json<UpdateNotificationSettingsRequest>,
) -> Response {
    let patch = match request.patch() {
        Ok(patch) => patch,
        Err(error) => return error.into_response(),
    };
    let (ip, device) = crate::api::audit::extract_ip_and_ua(&headers);
    let context = settings::SaveContext {
        encryption_secret: &state.encryption_key,
        smtp_available: state.smtp_config.is_some(),
        metadata: Metadata {
            ip,
            device,
            ..Default::default()
        },
    };
    match settings::save(
        &state.db_write,
        user_id,
        &patch,
        &context,
        |current, sensitive| {
            if claims.session_version != current.session_version {
                return Err(AppError::SessionRevoked("session is no longer valid".into()).into());
            }
            if sensitive {
                require_sudo_for_user(&claims, current, &state.jwt_secret)?;
            }
            Ok(())
        },
    )
    .await
    {
        Ok(settings) => Json(response(settings, state.smtp_config.is_some())).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestNotificationRequest {
    pub event: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in=Query)]
pub struct TestNotificationQuery {
    pub event_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NotificationChannelResult {
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TestNotificationResponse {
    pub event_id: String,
    pub channels: BTreeMap<String, NotificationChannelResult>,
}

fn test_response(
    event_id: String,
    rows: Vec<rg_db::entities::notification_delivery::Model>,
) -> TestNotificationResponse {
    TestNotificationResponse {
        event_id,
        channels: rows
            .into_iter()
            .map(|row| {
                (
                    row.channel,
                    NotificationChannelResult {
                        status: row.status,
                        reason: row.last_error,
                    },
                )
            })
            .collect(),
    }
}

#[utoipa::path(post,path="/users/me/notification-settings/test",tag="Notifications",request_body=TestNotificationRequest,responses(
    (status=200,description="Saved-settings test queued; poll for per-channel results",body=TestNotificationResponse),
    (status=400,description="Unknown event",body=serde_json::Value),
    (status=401,description="Login session required",body=serde_json::Value),
    (status=429,description="Test cooldown or pending test",body=serde_json::Value),
))]
pub async fn test_notification(
    State(state): State<AppState>,
    LoginSession { user_id, claims }: LoginSession,
    Json(request): Json<TestNotificationRequest>,
) -> Response {
    let Some(event) = Event::parse(&request.event) else {
        return AppError::bad_request("unknown notification event").into_response();
    };
    let result: Result<_, anyhow::Error> =
        rg_db::contention::retry_transaction("notification test", || async {
            let txn = state.db_write.begin().await?;
            let user = queue::lock_account(&txn, user_id)
                .await?
                .ok_or_else(|| AppError::unauthorized("account unavailable"))?;
            if claims.session_version != user.session_version {
                return Err(AppError::SessionRevoked("session is no longer valid".into()).into());
            }
            if !queue::test_allowed(&txn, user_id, event).await? {
                return Err(AppError::rate_limited("notification test cooldown").into());
            }
            let settings = settings::get(&txn, user_id).await?;
            let mut event = queue::QueueEvent::security(user_id, event, None);
            // Synthetic metadata never asks for an actual credential mutation.
            event.metadata = Metadata::default();
            event.is_test = true;
            let channels = settings.events[&event.event];
            for (channel, enabled, available, unavailable) in [
                (
                    "email",
                    channels.email,
                    state.smtp_config.is_some() && !user.email.is_empty(),
                    "smtp_unavailable",
                ),
                (
                    "telegram",
                    channels.telegram,
                    settings.profile.telegram_configured(),
                    "telegram_not_configured",
                ),
            ] {
                let mut row = queue::row(&event, &user, &settings.profile, channel)?;
                if !enabled || !available {
                    row.status = sea_orm::Set("unavailable".into());
                    row.last_error = sea_orm::Set(Some(
                        if enabled {
                            unavailable
                        } else {
                            "channel_disabled"
                        }
                        .into(),
                    ));
                    row.metadata = sea_orm::Set("{}".into());
                    row.destination_hash = sea_orm::Set(String::new());
                    row.completed_at = sea_orm::Set(Some(chrono::Utc::now()));
                }
                queue::insert(&txn, row).await?;
            }
            let rows = queue::event_rows(&txn, user_id, &event.id).await?;
            txn.commit().await?;
            Ok(test_response(event.id, rows))
        })
        .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => error_response(error),
    }
}

#[utoipa::path(get,path="/users/me/notification-settings/test",tag="Notifications",params(TestNotificationQuery),responses(
    (status=200,description="Caller-owned test results",body=TestNotificationResponse),
    (status=401,description="Login session required",body=serde_json::Value),
    (status=404,description="Test not found",body=serde_json::Value),
))]
pub async fn notification_test_result(
    State(state): State<AppState>,
    LoginSession { user_id, .. }: LoginSession,
    Query(query): Query<TestNotificationQuery>,
) -> Response {
    if query.event_id.len() > 80 {
        return AppError::not_found("test not found").into_response();
    }
    match queue::event_rows(&state.db_write, user_id, &query.event_id).await {
        Ok(rows) if !rows.is_empty() && rows.iter().all(|row| row.is_test) => {
            Json(test_response(query.event_id, rows)).into_response()
        }
        Ok(_) => AppError::not_found("test not found").into_response(),
        Err(error) => error_response(error),
    }
}
