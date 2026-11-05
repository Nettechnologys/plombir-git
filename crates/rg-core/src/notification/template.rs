//! Structured, localized notification text. User-provided names stay literal;
//! HTML is escaped only at the SMTP boundary. No credential-bearing payloads.
use rg_db::notification::{Event, Locale, Metadata};

pub struct Message {
    pub subject: String,
    pub text: String,
    pub html: String,
}

fn translated(
    locale: Locale,
    en: &'static str,
    ru: &'static str,
    zh: &'static str,
) -> &'static str {
    match locale {
        Locale::En => en,
        Locale::Ru => ru,
        Locale::ZhCn => zh,
    }
}

pub fn title(event: Event, locale: Locale) -> &'static str {
    let (en, ru, zh) = match event {
        Event::ReviewRequested => ("Review requested", "Запрошено ревью", "收到审查请求"),
        Event::Mention => ("You were mentioned", "Вас упомянули", "有人提及了你"),
        Event::Assigned => ("Issue assigned", "Вам назначена задача", "议题已分配给你"),
        Event::CiFailed => ("CI failed", "Ошибка CI", "CI 失败"),
        Event::Participating => ("Thread activity", "Обновление обсуждения", "讨论有新动态"),
        Event::CiTriggered => ("CI started", "Запущен CI", "CI 已启动"),
        Event::NewDevice => (
            "Sign-in from a new device",
            "Вход с нового устройства",
            "新设备登录",
        ),
        Event::TokenCreated => (
            "Access token created",
            "Создан токен доступа",
            "访问令牌已创建",
        ),
        Event::TokenRevoked => (
            "Access token revoked",
            "Отозван токен доступа",
            "访问令牌已撤销",
        ),
        Event::AgentTokenCreated => (
            "Agent token created",
            "Создан токен агента",
            "代理令牌已创建",
        ),
        Event::AgentTokenRevoked => (
            "Agent token revoked",
            "Отозван токен агента",
            "代理令牌已撤销",
        ),
        Event::SshKeyAdded => ("SSH key added", "Добавлен SSH-ключ", "SSH 密钥已添加"),
        Event::SshKeyRemoved => ("SSH key removed", "Удалён SSH-ключ", "SSH 密钥已删除"),
        Event::SigningKeyAdded => (
            "Signing key added",
            "Добавлен ключ подписи",
            "签名密钥已添加",
        ),
        Event::SigningKeyRemoved => (
            "Signing key removed",
            "Удалён ключ подписи",
            "签名密钥已删除",
        ),
        Event::DeployKeyAdded => (
            "Deploy key added",
            "Добавлен ключ развёртывания",
            "部署密钥已添加",
        ),
        Event::DeployKeyRemoved => (
            "Deploy key removed",
            "Удалён ключ развёртывания",
            "部署密钥已删除",
        ),
        Event::PasskeyAdded => ("Passkey added", "Добавлен ключ доступа", "通行密钥已添加"),
        Event::PasskeyRemoved => ("Passkey removed", "Удалён ключ доступа", "通行密钥已删除"),
        Event::MfaEnabled => (
            "Two-factor authentication enabled",
            "Включена двухфакторная аутентификация",
            "双重验证已启用",
        ),
        Event::MfaReplaced => (
            "Second factor replaced",
            "Заменён второй фактор",
            "第二验证因素已更换",
        ),
        Event::MfaDisabled => (
            "Two-factor authentication disabled",
            "Отключена двухфакторная аутентификация",
            "双重验证已停用",
        ),
        Event::BackupCodesRegenerated => (
            "Recovery codes replaced",
            "Перевыпущены резервные коды",
            "恢复代码已重新生成",
        ),
        Event::PasswordChanged => ("Password changed", "Пароль изменён", "密码已更改"),
        Event::PasswordReset => ("Password reset", "Пароль сброшен", "密码已重置"),
        Event::EmailChanged => (
            "Email address changed",
            "Адрес почты изменён",
            "电子邮件地址已更改",
        ),
        Event::ExternalLoginLinked => (
            "External sign-in linked",
            "Привязан внешний вход",
            "外部登录已关联",
        ),
        Event::ExternalLoginUnlinked => (
            "External sign-in unlinked",
            "Отвязан внешний вход",
            "外部登录已解除关联",
        ),
        Event::AdminPasswordReset => (
            "Administrator reset your password",
            "Администратор сбросил пароль",
            "管理员已重置你的密码",
        ),
        Event::AdminFactorsReset => (
            "Administrator reset authentication factors",
            "Администратор сбросил факторы входа",
            "管理员已重置验证因素",
        ),
        Event::RunnerTokenCreated => (
            "Runner token created",
            "Создан токен раннера",
            "运行器令牌已创建",
        ),
        Event::RunnerTokenRevoked => (
            "Runner token revoked",
            "Отозван токен раннера",
            "运行器令牌已撤销",
        ),
        Event::NotificationSettingsChanged => (
            "Notification settings changed",
            "Настройки уведомлений изменены",
            "通知设置已更改",
        ),
        Event::AgentProfileChanged => (
            "Agent profile changed",
            "Профиль агента изменён",
            "代理配置已更改",
        ),
        Event::BotCreated => ("Bot created", "Бот создан", "机器人已创建"),
        Event::BotDeleted => ("Bot deleted", "Бот удалён", "机器人已删除"),
    };
    translated(locale, en, ru, zh)
}

fn field_label(field: &str, locale: Locale) -> String {
    if let Some((event, channel)) = field.rsplit_once('.') {
        if let Some(event) = Event::parse(event) {
            return format!(
                "{} / {}",
                title(event, locale),
                if channel == "telegram" {
                    "Telegram"
                } else {
                    translated(locale, "Email", "Почта", "电子邮件")
                }
            );
        }
    }
    match field {
        "telegram.token" => translated(
            locale,
            "Telegram bot credential",
            "Токен бота Telegram",
            "Telegram 机器人凭证",
        ),
        "telegram.chat_id" => translated(locale, "Telegram chat", "Чат Telegram", "Telegram 聊天"),
        "telegram.message_thread_id" => {
            translated(locale, "Telegram topic", "Тема Telegram", "Telegram 话题")
        }
        "telegram.removed" => translated(
            locale,
            "Telegram disabled",
            "Telegram отключён",
            "Telegram 已停用",
        ),
        "locale" => translated(locale, "Language", "Язык", "语言"),
        "enabled" => translated(locale, "Agent enabled", "Агент включён", "代理启用状态"),
        "preset" => translated(locale, "Agent preset", "Профиль агента", "代理预设"),
        "image" => translated(locale, "Container image", "Образ контейнера", "容器镜像"),
        "command" => translated(locale, "Agent command", "Команда агента", "代理命令"),
        "runner_tags" => translated(locale, "Runner tags", "Метки раннера", "运行器标签"),
        "model_key_encrypted" => translated(locale, "Model credential", "Ключ модели", "模型凭据"),
        "timeout_secs" => translated(locale, "Run timeout", "Таймаут запуска", "运行超时"),
        "max_runs_per_day" => translated(
            locale,
            "Daily run limit",
            "Лимит запусков в день",
            "每日运行上限",
        ),
        "max_iterations" => translated(locale, "Iteration limit", "Лимит итераций", "迭代上限"),
        "repositories" => translated(
            locale,
            "Allowed repositories",
            "Разрешённые репозитории",
            "允许的仓库",
        ),
        "profile_removed" => translated(
            locale,
            "Agent profile removed",
            "Профиль агента удалён",
            "代理配置已删除",
        ),

        _ => field,
    }
    .to_owned()
}

fn limit(value: &str, max: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max
        })
        .collect()
}

pub fn render(
    event: Event,
    locale: Locale,
    metadata: &Metadata,
    at: chrono::DateTime<chrono::Utc>,
    is_test: bool,
    prior: bool,
    base_url: Option<&str>,
) -> Message {
    let heading = title(event, locale);
    let test = translated(locale, "TEST", "ТЕСТ", "测试");
    let subject = format!(
        "[Plombir Git] {}{heading}",
        if is_test {
            format!("[{test}] ")
        } else {
            String::new()
        }
    );
    let mut detail = format!(
        "{}: {} UTC\n",
        translated(locale, "Time", "Время", "时间"),
        at.format("%Y-%m-%d %H:%M:%S")
    );
    let method = metadata.method.as_deref().map(|value| {
        match value {
            "local" | "password" => translated(locale, "Password", "Пароль", "密码"),
            "password_reset" => translated(locale, "Password reset", "Сброс пароля", "密码重置"),
            "initial_password" => {
                translated(locale, "Initial password", "Начальный пароль", "初始密码")
            }
            "registration" => translated(locale, "Registration", "Регистрация", "注册"),
            "passkey" => translated(locale, "Passkey", "Ключ доступа", "通行密钥"),
            _ => value,
        }
        .to_owned()
    });
    for (value, en, ru, zh) in [
        (&metadata.actor, "Actor", "Кто", "操作者"),
        (&metadata.name, "Name", "Имя", "名称"),
        (&metadata.fingerprint, "Fingerprint", "Отпечаток", "指纹"),
        (&metadata.ip, "IP address", "IP-адрес", "IP 地址"),
        (&metadata.device, "Device", "Устройство", "设备"),
        (&method, "Sign-in method", "Способ входа", "登录方式"),
        (&metadata.subject, "Subject", "Тема", "主题"),
    ] {
        if let Some(value) = value {
            detail.push_str(&format!(
                "{}: {}\n",
                translated(locale, en, ru, zh),
                limit(value, 256)
            ));
        }
    }
    if !metadata.changed.is_empty() {
        detail.push_str(&format!(
            "{}: {}\n",
            translated(locale, "Changed", "Изменено", "更改内容"),
            metadata
                .changed
                .iter()
                .map(|key| field_label(key, locale))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let action = translated(locale, "Open settings", "Открыть настройки", "打开设置");
    let path = if prior {
        "/settings/notifications"
    } else {
        metadata.link.as_deref().unwrap_or(if event.security() {
            "/settings/security"
        } else {
            "/notifications"
        })
    };
    let link = base_url
        .map(|base| format!("{}{path}", base.trim_end_matches('/')))
        .unwrap_or_else(|| path.to_owned());
    let footer = if prior {
        translated(
            locale,
            "This notice was sent to your previously confirmed destination.",
            "Это предупреждение отправлено по прежнему подтверждённому назначению.",
            "此提醒发送至你之前确认的通知接收位置。",
        )
    } else {
        translated(
            locale,
            "You receive this notice according to your saved Plombir Git notification settings.",
            "Это уведомление отправлено по сохранённым настройкам уведомлений Plombir Git.",
            "此通知根据你保存的 Plombir Git 通知设置发送。",
        )
    };
    // Reserve space for the complete action/footer. UTF-16 is deliberately
    // conservative for Telegram's text bound; never split a Unicode scalar.
    let text = format!(
        "{subject}\n\n{}\n{action}: {}\n\n{footer}",
        limit(&detail, 2800),
        limit(&link, 600)
    );
    let html = format!("<html><body><pre style=\"white-space:pre-wrap;font-family:sans-serif\">{}</pre></body></html>", escape(&text));
    Message {
        subject,
        text,
        html,
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_events_have_complete_localized_bounded_templates_and_literal_names() {
        let metadata = Metadata {
            name: Some("<script>Alice & Bob</script>".into()),
            changed: Event::ALL
                .iter()
                .flat_map(|e| {
                    [
                        format!("{}.email", e.key()),
                        format!("{}.telegram", e.key()),
                    ]
                })
                .collect(),
            ..Default::default()
        };
        for &event in Event::ALL {
            let mut titles = std::collections::HashSet::new();
            for locale in [Locale::En, Locale::Ru, Locale::ZhCn] {
                assert!(titles.insert(title(event, locale)));
                let message = render(
                    event,
                    locale,
                    &metadata,
                    chrono::Utc::now(),
                    true,
                    false,
                    Some("https://forge.invalid"),
                );
                assert!(message.text.contains("<script>Alice & Bob</script>"));
                assert!(!message.html.contains("<script>"));
                assert!(message.text.encode_utf16().count() <= 4096);
                assert!(message.text.contains(translated(
                    locale,
                    "Open settings",
                    "Открыть настройки",
                    "打开设置"
                )));
                assert!(message
                    .text
                    .contains(translated(locale, "TEST", "ТЕСТ", "测试")));
            }
        }
    }
}
