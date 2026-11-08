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
        Event::RepositoryAccessChanged => (
            "Repository access changed",
            "Доступ к репозиторию изменён",
            "仓库访问权限已更改",
        ),
        Event::OrganizationAccessChanged => (
            "Organization access changed",
            "Доступ к организации изменён",
            "组织访问权限已更改",
        ),
        Event::RepositorySecurityChanged => (
            "Repository visibility, ownership or deletion",
            "Приватность, владелец или удаление репозитория",
            "仓库可见性、所有权或删除",
        ),
        Event::OrganizationSecurityChanged => (
            "Organization visibility or deletion",
            "Приватность или удаление организации",
            "组织可见性或删除",
        ),
        Event::WebhookChanged => (
            "Webhook configuration changed",
            "Настройки вебхука изменены",
            "Webhook 配置已更改",
        ),
        Event::MirrorChanged => (
            "Mirror configuration changed",
            "Настройки зеркала изменены",
            "镜像配置已更改",
        ),
        Event::CiSecurityChanged => (
            "Repository protection or CI configuration changed",
            "Защита репозитория или настройки CI изменены",
            "仓库保护或 CI 配置已更改",
        ),
        Event::AdminSecurityChanged => (
            "Administrator security action",
            "Действие администратора с безопасностью",
            "管理员安全操作",
        ),
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

        "collaborator_added" => translated(
            locale,
            "Collaborator added",
            "Участник добавлен",
            "协作者已添加",
        ),
        "collaborator_permission_changed" => translated(
            locale,
            "Collaborator permission changed",
            "Права участника изменены",
            "协作者权限已更改",
        ),
        "collaborator_removed" => translated(
            locale,
            "Collaborator removed",
            "Участник удалён",
            "协作者已移除",
        ),
        "org_member_added" => translated(
            locale,
            "Organization member added",
            "Участник организации добавлен",
            "组织成员已添加",
        ),
        "org_member_removed" => translated(
            locale,
            "Organization member and team grants removed",
            "Участник организации и его права в командах удалены",
            "组织成员及其团队权限已移除",
        ),
        "organization_deleted" => translated(
            locale,
            "Organization deleted",
            "Организация удалена",
            "组织已删除",
        ),
        "organization_visibility_changed" => translated(
            locale,
            "Organization visibility changed",
            "Приватность организации изменена",
            "组织可见性已更改",
        ),
        "repository_deleted" => translated(
            locale,
            "Repository deleted",
            "Репозиторий удалён",
            "仓库已删除",
        ),
        "repository_private" => translated(
            locale,
            "Repository made private",
            "Репозиторий сделан приватным",
            "仓库已设为私有",
        ),
        "repository_public" => translated(
            locale,
            "Repository made public",
            "Репозиторий сделан публичным",
            "仓库已设为公开",
        ),
        "repository_transferred" => translated(
            locale,
            "Repository transferred with its access and integration settings",
            "Репозиторий передан вместе с доступом и настройками интеграций",
            "仓库及其访问和集成设置已转移",
        ),
        "private_repository_forked" => translated(
            locale,
            "Private repository copied into a fork",
            "Создана копия приватного репозитория",
            "私有仓库已复制为分叉",
        ),
        "account_authority_changed" => translated(
            locale,
            "Account privileges or activation changed",
            "Полномочия или активность аккаунта изменены",
            "账户权限或启用状态已更改",
        ),
        "account_deleted" => translated(locale, "Account deleted", "Аккаунт удалён", "账户已删除"),
        "account_unlocked" => translated(
            locale,
            "Account unlocked",
            "Аккаунт разблокирован",
            "账户已解锁",
        ),
        "team_created" => translated(locale, "Team created", "Команда: создание", "团队：创建"),
        "team_updated" => translated(locale, "Team updated", "Команда: изменение", "团队：更改"),
        "team_deleted" => translated(locale, "Team deleted", "Команда: удаление", "团队：删除"),
        "webhook_created" => translated(
            locale,
            "Webhook created",
            "Вебхук: создание",
            "Webhook：创建",
        ),
        "webhook_updated" => translated(
            locale,
            "Webhook updated",
            "Вебхук: изменение",
            "Webhook：更改",
        ),
        "webhook_deleted" => translated(
            locale,
            "Webhook deleted",
            "Вебхук: удаление",
            "Webhook：删除",
        ),
        "pull_mirror_created" => translated(
            locale,
            "Pull mirror created",
            "Входящее зеркало: создание",
            "拉取镜像：创建",
        ),
        "pull_mirror_updated" => translated(
            locale,
            "Pull mirror updated",
            "Входящее зеркало: изменение",
            "拉取镜像：更改",
        ),
        "pull_mirror_deleted" => translated(
            locale,
            "Pull mirror deleted",
            "Входящее зеркало: удаление",
            "拉取镜像：删除",
        ),
        "push_mirror_created" => translated(
            locale,
            "Push mirror created",
            "Исходящее зеркало: создание",
            "推送镜像：创建",
        ),
        "push_mirror_updated" => translated(
            locale,
            "Push mirror updated",
            "Исходящее зеркало: изменение",
            "推送镜像：更改",
        ),
        "push_mirror_deleted" => translated(
            locale,
            "Push mirror deleted",
            "Исходящее зеркало: удаление",
            "推送镜像：删除",
        ),
        "ci_secret_created" => translated(
            locale,
            "CI secret created",
            "Секрет CI: создание",
            "CI 密钥：创建",
        ),
        "ci_secret_updated" => translated(
            locale,
            "CI secret updated",
            "Секрет CI: изменение",
            "CI 密钥：更改",
        ),
        "ci_secret_deleted" => translated(
            locale,
            "CI secret deleted",
            "Секрет CI: удаление",
            "CI 密钥：删除",
        ),
        "ci_variable_created" => translated(
            locale,
            "CI variable created",
            "Переменная CI: создание",
            "CI 变量：创建",
        ),
        "ci_variable_updated" => translated(
            locale,
            "CI variable updated",
            "Переменная CI: изменение",
            "CI 变量：更改",
        ),
        "ci_variable_deleted" => translated(
            locale,
            "CI variable deleted",
            "Переменная CI: удаление",
            "CI 变量：删除",
        ),
        "ci_environment_created" => translated(
            locale,
            "CI environment created",
            "Окружение CI: создание",
            "CI 环境：创建",
        ),
        "ci_environment_updated" => translated(
            locale,
            "CI environment updated",
            "Окружение CI: изменение",
            "CI 环境：更改",
        ),
        "ci_environment_deleted" => translated(
            locale,
            "CI environment deleted",
            "Окружение CI: удаление",
            "CI 环境：删除",
        ),
        "branch_protection_created" => translated(
            locale,
            "Branch protection created",
            "Защита веток: создание",
            "分支保护：创建",
        ),
        "branch_protection_updated" => translated(
            locale,
            "Branch protection updated",
            "Защита веток: изменение",
            "分支保护：更改",
        ),
        "branch_protection_deleted" => translated(
            locale,
            "Branch protection deleted",
            "Защита веток: удаление",
            "分支保护：删除",
        ),
        "tag_protection_created" => translated(
            locale,
            "Tag protection created",
            "Защита тегов: создание",
            "标签保护：创建",
        ),
        "tag_protection_updated" => translated(
            locale,
            "Tag protection updated",
            "Защита тегов: изменение",
            "标签保护：更改",
        ),
        "tag_protection_deleted" => translated(
            locale,
            "Tag protection deleted",
            "Защита тегов: удаление",
            "标签保护：删除",
        ),
        "sso_provider_created" => translated(
            locale,
            "Sign-in provider created",
            "Провайдер входа: создание",
            "登录提供商：创建",
        ),
        "sso_provider_updated" => translated(
            locale,
            "Sign-in provider updated",
            "Провайдер входа: изменение",
            "登录提供商：更改",
        ),
        "sso_provider_deleted" => translated(
            locale,
            "Sign-in provider deleted",
            "Провайдер входа: удаление",
            "登录提供商：删除",
        ),
        "team_member_added" => translated(
            locale,
            "Team member added",
            "Участник команды добавлен",
            "团队成员已添加",
        ),
        "team_member_removed" => translated(
            locale,
            "Team member removed",
            "Участник команды удалён",
            "团队成员已移除",
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
