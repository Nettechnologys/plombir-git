//! InstanceSettings entity — maps to the `instance_settings` table.
//! The single row (id 1) holding this instance's admin-toggled switches.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The primary key of the one row this table ever holds.
pub const SINGLETON_ID: i64 = 1;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "instance_settings")]
pub struct Model {
    /// Always [`SINGLETON_ID`] — assigned by the writer, never generated.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// When true, only GET/HEAD/OPTIONS and admin routes are served.
    pub maintenance_mode: bool,
    /// `None` means no banner is shown.
    pub banner_message: Option<String>,
    /// Banner type: "info", "warning", "error".
    pub banner_type: String,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
