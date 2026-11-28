//! What the one job of an agent run is handed when a runner takes it
//! (ADR-0001 D2, D5; card_47195d097b79, card_f42b96e48e66).
//!
//! A run's job gets exactly two secrets — the bot owner's model key and a
//! token minted for this attempt — and none of the repository's (both runners
//! ask [`crate::ci::receives_ci_secrets`] first). The contract, in one place
//! for the embedded runner and the external one:
//!
//! - both go into the job's **environment** only, never its argv, never the
//!   stored `script` / `variables` of the job, and both are in the log mask;
//! - only the attempt that is the run's latest and still open gets them: a job
//!   of an attempt the run has moved past, or of a run that ended, is handed
//!   nothing and settled failed;
//! - the token is minted when the job is handed out, not when the run is
//!   queued, and its row is written only by the hand-out that wins: the
//!   embedded runner writes it before it starts the container, the external
//!   one in the transaction that claims the job
//!   (`rg_db::ops::pipeline_ops::assign_job_with_mask_snapshot`). An earlier token of the
//!   same attempt (a job handed out again) is deleted in the same write.
//!
//! The rest of the environment tells the job where it is:
//! `PLOMBIR_SERVER_URL`, `PLOMBIR_MCP_URL`, `PLOMBIR_REPO_URL`, the commit and
//! the task the server wrote (`super::trust`).

use anyhow::{Context, Result};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};

use rg_db::entities::{agent_run, pipeline};

use super::profile::Preset;

/// The run token.
pub const TOKEN_VARIABLE: &str = "PLOMBIR_AGENT_TOKEN";
/// The model key, whatever the preset.
pub const MODEL_KEY_VARIABLE: &str = "PLOMBIR_MODEL_KEY";
/// The task the server wrote for the run.
pub const TASK_VARIABLE: &str = "PLOMBIR_TASK";

/// The shell the run's one job executes: [`PROLOGUE`], then `command`.
pub fn job_script(command: &str) -> String {
    format!("{PROLOGUE}\n# The agent.\n{command}\n")
}

/// What every run's job does before the agent: clone the repository with the
/// run's token at the commit the run recorded, and write the task and the MCP
/// configuration — all outside the runner's workspace, which on the external
/// runner has no `.git` (`git archive`) and on the embedded one has a `.git`
/// pointing at the server's bare repository.
///
/// The token travels as an `http.extraHeader` in git's environment
/// (`GIT_CONFIG_*`), never in the clone URL or a file in the repository, so
/// `git remote -v` and the clone's `.git/config` hold no credential. The CLI
/// a preset installs goes under a prefix the job's unprivileged user owns.
pub const PROLOGUE: &str = r#"set -eu
# plombir-git agent prologue (ADR-0001 D7, card_f42b96e48e66).
command -v git >/dev/null 2>&1 || { echo "plombir-git: the agent's image has no git" >&2; exit 127; }
umask 077
PLOMBIR_AGENT_HOME="${TMPDIR:-/tmp}/plombir-agent"
PLOMBIR_WORKDIR="$PLOMBIR_AGENT_HOME/repo"
PLOMBIR_TASK_FILE="$PLOMBIR_AGENT_HOME/task.md"
PLOMBIR_MCP_CONFIG="$PLOMBIR_AGENT_HOME/mcp.json"
NPM_CONFIG_PREFIX="$PLOMBIR_AGENT_HOME/npm"
PATH="$NPM_CONFIG_PREFIX/bin:$PATH"
export PLOMBIR_WORKDIR PLOMBIR_TASK_FILE PLOMBIR_MCP_CONFIG NPM_CONFIG_PREFIX PATH
rm -rf "$PLOMBIR_AGENT_HOME"
mkdir -p "$PLOMBIR_AGENT_HOME"
printf '%s
' "$PLOMBIR_TASK" > "$PLOMBIR_TASK_FILE"
printf '{"mcpServers":{"plombir":{"type":"http","url":"%s","headers":{"Authorization":"Bearer %s"}}}}
'   "$PLOMBIR_MCP_URL" "$PLOMBIR_AGENT_TOKEN" > "$PLOMBIR_MCP_CONFIG"
GIT_CONFIG_COUNT=1
GIT_CONFIG_KEY_0=http.extraHeader
GIT_CONFIG_VALUE_0="Authorization: Basic $(printf '%s:%s' "$PLOMBIR_AGENT_BOT" "$PLOMBIR_AGENT_TOKEN" | base64 | tr -d '
')"
export GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0
git clone --quiet --no-checkout "$PLOMBIR_REPO_URL" "$PLOMBIR_WORKDIR"
cd "$PLOMBIR_WORKDIR"
git checkout --quiet --detach "$PLOMBIR_COMMIT_SHA"
git config user.name "$PLOMBIR_AGENT_BOT"
git config user.email "$PLOMBIR_AGENT_BOT@agents.invalid"
"#;

/// The model key under the name a preset's CLI reads it by, besides
/// [`MODEL_KEY_VARIABLE`].
fn preset_key_variables(preset: Preset) -> &'static [&'static str] {
    match preset {
        Preset::ClaudeCode => &["ANTHROPIC_API_KEY"],
        Preset::Codex => &["CODEX_API_KEY", "OPENAI_API_KEY"],
        Preset::Custom => &[],
    }
}

/// A token for the attempt, generated but not yet stored: the row is written
/// by whichever hand-out of the job wins ([`store_token_in`]).
#[derive(Clone)]
pub struct PreparedToken {
    raw: String,
    attempt_id: i64,
    run_id: i64,
    row: crate::auth::access_token::NewAccessToken,
}

impl std::fmt::Debug for PreparedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedToken")
            .field("attempt_id", &self.attempt_id)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

/// What an agent run's job is handed on top of what every job gets.
#[derive(Debug)]
pub struct JobCredentials {
    /// Name → value, for the job's environment.
    pub variables: Vec<(String, String)>,
    /// Values the log mask hides: the token and the model key.
    pub masked: Vec<String>,
    /// The token's row, for the hand-out to store.
    pub token: PreparedToken,
}

/// Why an agent run's job gets nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotHandedOut(pub String);

/// The credentials and environment of the job of `pipeline`, if it is an
/// agent run's (`Ok(None)` for any other pipeline).
///
/// `Ok(Some(Err(..)))` is a job that must not run: its attempt is not the
/// run's current one, or the run is over. The caller settles it failed with
/// that reason rather than handing it out.
pub async fn job_credentials(
    db: &DatabaseConnection,
    encryption_key: &str,
    server_url: Option<&str>,
    pipeline: &pipeline::Model,
) -> Result<Option<std::result::Result<JobCredentials, NotHandedOut>>> {
    if pipeline.trigger_type != crate::ci::AGENT_RUN_EVENT {
        return Ok(None);
    }
    let refuse = |why: &str| Ok(Some(Err(NotHandedOut(why.to_string()))));
    let Some((run, attempt)) =
        rg_db::ops::agent_run_ops::attempt_of_pipeline(db, pipeline.id).await?
    else {
        return refuse("this pipeline carries no attempt of an agent run");
    };
    if run.active != Some(true) || attempt.finished_at.is_some() {
        return refuse("the agent run has ended");
    }
    let latest = rg_db::ops::agent_run_ops::attempts_of_run(db, run.id)
        .await?
        .into_iter()
        .map(|attempt| attempt.id)
        .max();
    if latest != Some(attempt.id) {
        return refuse("a later attempt of the agent run has replaced this one");
    }
    let Some(server_url) = server_url.map(|url| url.trim_end_matches('/').to_string()) else {
        return refuse("the server has no external_url, so the agent could not reach it");
    };
    let bot = rg_db::ops::user_ops::find_by_id(db, run.bot_id)
        .await?
        .context("the agent run's bot is gone")?;
    let Some(saved) = super::profile::get(db, &bot).await? else {
        return refuse("the bot has no agent profile any more");
    };
    let preset = Preset::parse(&saved.profile.preset)?;
    let key = crate::auth::encryption::derive_key(encryption_key);
    let model_key = saved
        .profile
        .model_key_encrypted
        .as_deref()
        .map(|sealed| crate::auth::encryption::decrypt(sealed, &key))
        .transpose()
        .context("unseal the bot's model key")?;
    let (owner, name) = crate::repo::service::repository_identity(db, run.repository_id).await?;
    let prefix = run
        .write_ref_prefix
        .clone()
        .unwrap_or_else(|| super::run::branch_prefix(&bot.username));
    let timeout_secs = saved.profile.timeout_secs;

    let grant = crate::auth::access_token::AgentRunGrant {
        bot: &bot,
        repository_id: run.repository_id,
        attempt_id: attempt.id,
        write_ref_prefix: &prefix,
        timeout_secs,
    };
    let row = crate::auth::access_token::agent_run_token_row(grant, &run)?;
    let raw = crate::auth::access_token::generate_access_token();

    let mut variables = vec![
        (TOKEN_VARIABLE.to_string(), raw.clone()),
        ("PLOMBIR_SERVER_URL".to_string(), server_url.clone()),
        (
            "PLOMBIR_MCP_URL".to_string(),
            format!("{server_url}/api/v1/mcp"),
        ),
        (
            "PLOMBIR_REPO_URL".to_string(),
            format!("{server_url}/{owner}/{name}.git"),
        ),
        ("PLOMBIR_REPOSITORY".to_string(), format!("{owner}/{name}")),
        (
            "PLOMBIR_COMMIT_SHA".to_string(),
            run.commit_sha
                .clone()
                .unwrap_or_else(|| pipeline.commit_sha.clone()),
        ),
        ("PLOMBIR_AGENT_BOT".to_string(), bot.username.clone()),
        (
            "PLOMBIR_AGENT_BRANCH_PREFIX".to_string(),
            prefix
                .strip_prefix("refs/heads/")
                .unwrap_or(&prefix)
                .to_string(),
        ),
        (
            TASK_VARIABLE.to_string(),
            run.task.clone().unwrap_or_default(),
        ),
    ];
    if let Some(issue_number) = issue_number(db, &run).await? {
        variables.push(("PLOMBIR_ISSUE_NUMBER".to_string(), issue_number.to_string()));
    }
    let mut masked = vec![raw.clone()];
    if let Some(model_key) = model_key {
        variables.push((MODEL_KEY_VARIABLE.to_string(), model_key.clone()));
        for name in preset_key_variables(preset) {
            variables.push((name.to_string(), model_key.clone()));
        }
        masked.push(model_key);
    }
    Ok(Some(Ok(JobCredentials {
        variables,
        masked,
        token: PreparedToken {
            raw,
            attempt_id: attempt.id,
            run_id: run.id,
            row,
        },
    })))
}

async fn issue_number(db: &DatabaseConnection, run: &agent_run::Model) -> Result<Option<i64>> {
    Ok(match run.issue_id {
        Some(issue_id) => rg_db::ops::issue_ops::find_by_id(db, issue_id)
            .await?
            .map(|issue| issue.number),
        None => None,
    })
}

/// Store `token` in `db` — the hand-out's own transaction — replacing any
/// earlier token of its attempt, and mark the run `running`.
pub async fn store_token_in(db: &impl ConnectionTrait, token: &PreparedToken) -> Result<()> {
    crate::auth::access_token::revoke_for_agent_run_attempt(db, token.attempt_id).await?;
    crate::auth::access_token::store_prepared(db, &token.raw, token.row.clone()).await?;
    agent_run::Entity::update_many()
        .col_expr(
            agent_run::Column::Status,
            sea_orm::sea_query::Expr::value("running"),
        )
        .filter(agent_run::Column::Id.eq(token.run_id))
        .filter(agent_run::Column::Status.eq("queued"))
        .exec(db)
        .await
        .context("db: mark an agent run running")?;
    Ok(())
}

/// The claim-time write of an external runner's hand-out of `token`
/// ([`rg_db::ops::pipeline_ops::assign_job_with_mask_snapshot`]).
pub fn hand_out(token: PreparedToken) -> Box<rg_db::ops::pipeline_ops::HandOutFn> {
    Box::new(move |tx| {
        let token = token.clone();
        Box::pin(async move { store_token_in(tx, &token).await })
    })
}

/// [`store_token_in`] in a transaction of its own, for the embedded runner,
/// which hands its job to nobody but itself.
pub async fn store_token(db: &DatabaseConnection, token: &PreparedToken) -> Result<()> {
    use sea_orm::TransactionTrait;
    rg_db::contention::retry_transaction("store an agent run token", || async {
        let txn = db.begin().await.context("db: begin storing a run token")?;
        match store_token_in(&txn, token).await {
            Ok(()) => txn
                .commit()
                .await
                .context("db: commit the run token"),
            Err(error) => {
                if let Err(rollback) = txn.rollback().await {
                    tracing::warn!(error = %format!("{rollback:#}"), "a run token write could not be rolled back");
                }
                Err(error)
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_preset_reads_the_key_by_its_own_name() {
        assert_eq!(
            preset_key_variables(Preset::ClaudeCode),
            &["ANTHROPIC_API_KEY"]
        );
        assert!(preset_key_variables(Preset::Codex).contains(&"CODEX_API_KEY"));
        assert!(preset_key_variables(Preset::Custom).is_empty());
    }
}
