//! A bot's agent profile: how it works when a person assigns it an issue
//! (ADR-0001 D5, D6, D9; card_d4d689acfa70).
//!
//! Only the bot's owner reads or writes it. The model key goes in sealed and
//! never comes out through here: [`SavedProfile`] carries the row, and the row
//! carries ciphertext, so the API can say whether a key is set and nothing
//! else.
//!
//! The repository list is the owner's consent. A repository admin can add
//! anybody's bot as a collaborator; a run starts only in a repository on this
//! list, so nobody but the owner decides where the owner's model key is spent.
//! Which repositories may go on it — ones the owner can read — is checked by
//! the caller, which resolves the names.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::entities::agent_profile::Model as AgentProfile;
use rg_db::entities::user::Model as User;
use rg_db::ops::agent_profile_ops;

use crate::error::{forbidden, invalid_request};

/// How long a run may take when the profile does not say: 30 minutes.
pub const DEFAULT_TIMEOUT_SECS: i64 = 30 * 60;
/// The longest run a profile may ask for: 4 hours.
pub const MAX_TIMEOUT_SECS: i64 = 4 * 60 * 60;
/// Runs a bot may start in a day when the profile does not say.
pub const DEFAULT_MAX_RUNS_PER_DAY: i64 = 20;
/// The most runs a day a profile may allow.
pub const MAX_RUNS_PER_DAY: i64 = 500;
/// Review iterations on one pull request when the profile does not say.
pub const DEFAULT_MAX_ITERATIONS: i64 = 5;
/// The most iterations on one pull request a profile may allow.
pub const MAX_ITERATIONS: i64 = 50;
/// Repositories one profile may list — the ceiling a narrowed token has.
pub const MAX_REPOSITORIES: usize = 100;
/// Runner labels one profile may require.
pub const MAX_RUNNER_TAGS: usize = 16;
/// The command of a `custom` profile.
pub const MAX_COMMAND_CHARS: usize = 8192;
/// A model API key. Real ones are a few hundred characters at most.
pub const MAX_MODEL_KEY_CHARS: usize = 4096;

/// What the bot runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Claude Code, headless.
    ClaudeCode,
    /// OpenAI Codex, `codex exec`.
    Codex,
    /// The owner's own image and command.
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::ClaudeCode, Preset::Codex, Preset::Custom];

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::ClaudeCode => "claude_code",
            Preset::Codex => "codex",
            Preset::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        Preset::ALL
            .into_iter()
            .find(|preset| preset.as_str() == value)
            .ok_or_else(|| {
                invalid_request(format!(
                    "preset must be one of {}",
                    Preset::ALL.map(Preset::as_str).join(", ")
                ))
            })
    }
}

/// What a write does to the stored model key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelKey {
    /// Leave it as it is — the default, so a form that never shows the key
    /// cannot erase it by saving.
    Keep,
    /// Replace it.
    Set(String),
    /// Remove it.
    Clear,
}

/// A complete profile as the owner submits it.
///
/// Every field but the key is the new value: an omitted limit is the default,
/// not "unchanged", so what the owner saw in the form is what is stored.
#[derive(Clone, Debug)]
pub struct ProfileUpdate {
    pub enabled: bool,
    pub preset: String,
    pub image: Option<String>,
    pub command: Option<String>,
    pub runner_tags: Option<Vec<String>>,
    pub model_key: ModelKey,
    pub timeout_secs: Option<i64>,
    pub max_runs_per_day: Option<i64>,
    pub max_iterations: Option<i64>,
    /// Already resolved and checked by the caller (see the module docs).
    pub repository_ids: Vec<i64>,
}

/// A stored profile and the repositories it may work in.
#[derive(Clone, Debug)]
pub struct SavedProfile {
    pub profile: AgentProfile,
    pub repository_ids: Vec<i64>,
}

impl SavedProfile {
    pub fn has_model_key(&self) -> bool {
        self.profile.model_key_encrypted.is_some()
    }

    /// The stored labels, split back out of the column.
    pub fn runner_tags(&self) -> Vec<String> {
        split_tags(self.profile.runner_tags.as_deref())
    }
}

fn split_tags(stored: Option<&str>) -> Vec<String> {
    stored
        .map(|tags| tags.split(',').map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The profile of `bot`, if it has one.
pub async fn get(db: &DatabaseConnection, bot: &User) -> Result<Option<SavedProfile>> {
    let Some(profile) = agent_profile_ops::find_by_bot(db, bot.id).await? else {
        return Ok(None);
    };
    let repository_ids = agent_profile_ops::repository_ids(db, profile.id).await?;
    Ok(Some(SavedProfile {
        profile,
        repository_ids,
    }))
}

/// Create or replace the profile of `bot`.
///
/// `encryption_key` is [`crate::auth::encryption::derive_key`] of the
/// instance's `[auth].encryption_key`, the key every other sealed column uses.
pub async fn save(
    db: &DatabaseConnection,
    encryption_key: &[u8; 32],
    bot: &User,
    update: ProfileUpdate,
) -> Result<SavedProfile> {
    if !bot.is_bot() {
        return Err(forbidden("only a bot account can have an agent profile"));
    }
    let existing = agent_profile_ops::find_by_bot(db, bot.id).await?;
    let row = validated_row(encryption_key, bot.id, existing.as_ref(), &update)?;
    let profile = agent_profile_ops::upsert(db, row, &update.repository_ids).await?;
    let repository_ids = agent_profile_ops::repository_ids(db, profile.id).await?;
    Ok(SavedProfile {
        profile,
        repository_ids,
    })
}

/// Delete the profile of `bot`; `false` when it had none.
pub async fn delete(db: &DatabaseConnection, bot: &User) -> Result<bool> {
    agent_profile_ops::delete_by_bot(db, bot.id).await
}

/// The row `update` stores, or the first rule it breaks.
///
/// Pure apart from sealing the key, so every refusal is reachable from a unit
/// test without a database.
fn validated_row(
    encryption_key: &[u8; 32],
    bot_id: i64,
    existing: Option<&AgentProfile>,
    update: &ProfileUpdate,
) -> Result<AgentProfile> {
    let preset = Preset::parse(update.preset.trim())?;

    let image = trimmed(update.image.as_deref());
    if let Some(image) = image.as_deref() {
        rg_process::image_reference::validate_image_reference(image).map_err(invalid_request)?;
    }
    let command = trimmed(update.command.as_deref());
    if let Some(command) = command.as_deref() {
        crate::text_limits::check("command", command, MAX_COMMAND_CHARS)?;
    }
    match preset {
        Preset::Custom => {
            if image.is_none() {
                return Err(invalid_request("a custom profile needs an image"));
            }
            if command.is_none() {
                return Err(invalid_request("a custom profile needs a command"));
            }
        }
        Preset::ClaudeCode | Preset::Codex => {
            if command.is_some() {
                return Err(invalid_request(format!(
                    "the {} preset runs its own command; choose the custom preset to set one",
                    preset.as_str()
                )));
            }
        }
    }

    let runner_tags = match update.runner_tags.as_deref() {
        None => None,
        Some(tags) => runner_tags(tags)?,
    };

    let timeout_secs = limit(
        "timeout_secs",
        update.timeout_secs,
        DEFAULT_TIMEOUT_SECS,
        60,
        MAX_TIMEOUT_SECS,
    )?;
    let max_runs_per_day = limit(
        "max_runs_per_day",
        update.max_runs_per_day,
        DEFAULT_MAX_RUNS_PER_DAY,
        1,
        MAX_RUNS_PER_DAY,
    )?;
    let max_iterations = limit(
        "max_iterations",
        update.max_iterations,
        DEFAULT_MAX_ITERATIONS,
        1,
        MAX_ITERATIONS,
    )?;

    if update.repository_ids.len() > MAX_REPOSITORIES {
        return Err(invalid_request(format!(
            "an agent profile may list at most {MAX_REPOSITORIES} repositories"
        )));
    }

    let model_key_encrypted = match &update.model_key {
        ModelKey::Keep => existing.and_then(|profile| profile.model_key_encrypted.clone()),
        ModelKey::Clear => None,
        ModelKey::Set(key) => Some(seal_model_key(encryption_key, key)?),
    };
    // A preset reaches a hosted model and cannot start without a key; a custom
    // command may bring its own way to a model, so it is not held to this.
    if update.enabled && preset != Preset::Custom && model_key_encrypted.is_none() {
        return Err(invalid_request(format!(
            "the {} preset needs a model key before the profile can be enabled",
            preset.as_str()
        )));
    }

    let now = chrono::Utc::now();
    Ok(AgentProfile {
        id: existing.map_or(0, |profile| profile.id),
        bot_id,
        enabled: update.enabled,
        preset: preset.as_str().to_owned(),
        image,
        command,
        runner_tags,
        model_key_encrypted,
        timeout_secs,
        max_runs_per_day,
        max_iterations,
        created_at: existing.map_or(now, |profile| profile.created_at),
        updated_at: now,
    })
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The labels as stored, or `None` for an empty list.
fn runner_tags(tags: &[String]) -> Result<Option<String>> {
    if tags.len() > MAX_RUNNER_TAGS {
        return Err(invalid_request(format!(
            "an agent profile may require at most {MAX_RUNNER_TAGS} runner labels"
        )));
    }
    let mut cleaned: Vec<&str> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(invalid_request("a runner label cannot be empty"));
        }
        crate::text_limits::check("runner label", tag, crate::text_limits::MAX_LINE_CHARS)?;
        if tag.contains(',') || tag.chars().any(char::is_control) {
            return Err(invalid_request(format!(
                "runner label '{}' may not contain a comma or a control character",
                tag.escape_debug()
            )));
        }
        if !cleaned.contains(&tag) {
            cleaned.push(tag);
        }
    }
    Ok((!cleaned.is_empty()).then(|| cleaned.join(",")))
}

fn limit(field: &str, value: Option<i64>, default: i64, min: i64, max: i64) -> Result<i64> {
    let value = value.unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(invalid_request(format!(
            "{field} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

fn seal_model_key(encryption_key: &[u8; 32], key: &str) -> Result<String> {
    // Keys are pasted; a stray newline or space around one is the paste, not
    // the key. Anything inside it is refused rather than quietly sent.
    let key = key.trim();
    if key.is_empty() {
        return Err(invalid_request(
            "model_key cannot be empty; send null to keep the stored key or clear_model_key to remove it",
        ));
    }
    crate::text_limits::check("model_key", key, MAX_MODEL_KEY_CHARS)?;
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid_request(
            "model_key may not contain whitespace or control characters",
        ));
    }
    crate::auth::encryption::encrypt(key, encryption_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    fn update(preset: &str) -> ProfileUpdate {
        ProfileUpdate {
            enabled: false,
            preset: preset.to_owned(),
            image: None,
            command: None,
            runner_tags: None,
            model_key: ModelKey::Keep,
            timeout_secs: None,
            max_runs_per_day: None,
            max_iterations: None,
            repository_ids: Vec::new(),
        }
    }

    fn refusal(update: &ProfileUpdate) -> String {
        format!(
            "{:#}",
            validated_row(&KEY, 1, None, update).expect_err("the update must be refused")
        )
    }

    #[test]
    fn a_preset_without_overrides_takes_every_default() {
        let row = validated_row(&KEY, 1, None, &update("claude_code")).unwrap();
        assert_eq!(row.preset, "claude_code");
        assert_eq!(row.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(row.max_runs_per_day, DEFAULT_MAX_RUNS_PER_DAY);
        assert_eq!(row.max_iterations, DEFAULT_MAX_ITERATIONS);
        assert_eq!(
            (row.image, row.command, row.runner_tags),
            (None, None, None)
        );
        assert!(!row.enabled);
    }

    #[test]
    fn an_unknown_preset_is_refused_by_name() {
        assert!(refusal(&update("gemini")).contains("claude_code, codex, custom"));
    }

    #[test]
    fn a_custom_profile_needs_an_image_and_a_command() {
        let mut custom = update("custom");
        assert!(refusal(&custom).contains("needs an image"));
        custom.image = Some("ghcr.io/acme/agent:1".into());
        assert!(refusal(&custom).contains("needs a command"));
        custom.command = Some("run-agent --task \"$PLOMBIR_TASK_FILE\"".into());
        assert!(validated_row(&KEY, 1, None, &custom).is_ok());
    }

    #[test]
    fn a_preset_does_not_take_a_command() {
        let mut preset = update("codex");
        preset.command = Some("rm -rf /".into());
        assert!(refusal(&preset).contains("runs its own command"));
    }

    #[test]
    fn an_image_that_is_not_a_reference_is_refused() {
        let mut preset = update("claude_code");
        preset.image = Some("--privileged".into());
        assert!(refusal(&preset).contains("is not an image reference"));
    }

    #[test]
    fn enabling_a_preset_needs_a_key_but_a_custom_command_does_not() {
        let mut preset = update("claude_code");
        preset.enabled = true;
        assert!(refusal(&preset).contains("needs a model key"));
        preset.model_key = ModelKey::Set("sk-test-0123456789".into());
        assert!(validated_row(&KEY, 1, None, &preset).unwrap().enabled);

        let mut custom = update("custom");
        custom.enabled = true;
        custom.image = Some("ghcr.io/acme/agent:1".into());
        custom.command = Some("run-agent".into());
        assert!(validated_row(&KEY, 1, None, &custom).unwrap().enabled);
    }

    #[test]
    fn the_key_is_stored_sealed_and_opens_to_what_was_pasted() {
        let mut preset = update("codex");
        preset.model_key = ModelKey::Set("  sk-test-0123456789\n".into());
        let row = validated_row(&KEY, 1, None, &preset).unwrap();
        let sealed = row.model_key_encrypted.expect("a key was set");
        assert!(!sealed.contains("sk-test"));
        assert_eq!(
            crate::auth::encryption::decrypt(&sealed, &KEY).unwrap(),
            "sk-test-0123456789"
        );
    }

    #[test]
    fn keep_keeps_and_clear_clears_the_stored_key() {
        let mut first = update("codex");
        first.model_key = ModelKey::Set("sk-test-0123456789".into());
        let stored = validated_row(&KEY, 1, None, &first).unwrap();

        let kept = validated_row(&KEY, 1, Some(&stored), &update("codex")).unwrap();
        assert_eq!(kept.model_key_encrypted, stored.model_key_encrypted);
        assert_eq!(kept.created_at, stored.created_at);

        let mut clear = update("codex");
        clear.model_key = ModelKey::Clear;
        assert_eq!(
            validated_row(&KEY, 1, Some(&stored), &clear)
                .unwrap()
                .model_key_encrypted,
            None
        );
    }

    #[test]
    fn a_key_with_inner_whitespace_or_an_empty_key_is_refused() {
        let mut preset = update("codex");
        preset.model_key = ModelKey::Set("sk test".into());
        assert!(refusal(&preset).contains("whitespace"));
        preset.model_key = ModelKey::Set("   ".into());
        assert!(refusal(&preset).contains("cannot be empty"));
    }

    #[test]
    fn limits_outside_their_range_are_refused_by_field() {
        for (field, set) in [
            ("timeout_secs", 59_i64),
            ("timeout_secs", MAX_TIMEOUT_SECS + 1),
            ("max_runs_per_day", 0),
            ("max_iterations", MAX_ITERATIONS + 1),
        ] {
            let mut preset = update("claude_code");
            match field {
                "timeout_secs" => preset.timeout_secs = Some(set),
                "max_runs_per_day" => preset.max_runs_per_day = Some(set),
                _ => preset.max_iterations = Some(set),
            }
            assert!(refusal(&preset).contains(field), "{field} = {set}");
        }
    }

    #[test]
    fn runner_labels_are_trimmed_deduplicated_and_refused_with_a_comma() {
        let mut preset = update("claude_code");
        preset.runner_tags = Some(vec![" gpu ".into(), "linux".into(), "gpu".into()]);
        let row = validated_row(&KEY, 1, None, &preset).unwrap();
        assert_eq!(row.runner_tags.as_deref(), Some("gpu,linux"));

        preset.runner_tags = Some(vec!["a,b".into()]);
        assert!(refusal(&preset).contains("comma"));
        preset.runner_tags = Some(vec!["".into()]);
        assert!(refusal(&preset).contains("cannot be empty"));
        preset.runner_tags = Some(vec![]);
        assert_eq!(
            validated_row(&KEY, 1, None, &preset).unwrap().runner_tags,
            None
        );
    }

    #[test]
    fn more_repositories_than_a_narrowed_token_may_name_are_refused() {
        let mut preset = update("claude_code");
        preset.repository_ids = (1..=(MAX_REPOSITORIES as i64 + 1)).collect();
        assert!(refusal(&preset).contains("at most 100 repositories"));
    }
}
