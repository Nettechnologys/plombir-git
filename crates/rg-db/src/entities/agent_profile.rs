//! How a bot works as an agent when a person assigns it an issue — maps to the
//! `agent_profiles` table (ADR-0001, card_d4d689acfa70).
//!
//! One row per bot (`bot_id` is unique), deleted with the bot. The model
//! key is ciphertext from `rg_core::auth::encryption` and is never returned by
//! the API; `model_key_encrypted.is_some()` is all a reader learns.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_profiles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub bot_id: i64,
    /// The owner's switch. A disabled profile is kept, and assigning the bot
    /// starts nothing.
    pub enabled: bool,
    /// `claude_code`, `codex` or `custom`.
    pub preset: String,
    /// The image the run starts from; `None` is the preset's.
    pub image: Option<String>,
    /// The command a `custom` profile runs; presets carry their own.
    pub command: Option<String>,
    /// Comma-joined runner labels the run needs; `None` is the preset's.
    pub runner_tags: Option<String>,
    pub model_key_encrypted: Option<String>,
    pub timeout_secs: i64,
    pub max_runs_per_day: i64,
    pub max_iterations: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::BotId",
        to = "super::user::Column::Id"
    )]
    Bot,
}

impl ActiveModelBehavior for ActiveModel {}
