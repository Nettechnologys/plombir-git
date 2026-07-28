//! Instance-wide settings (maintenance mode, banner).
//!
//! These describe one running instance, so they live in that instance's
//! database and are cached by the process state that serves it.

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct InstanceSettings {
    /// When true, only read-only operations and admin settings updates are served.
    pub maintenance_mode: bool,
    /// Optional banner message shown to all users.
    pub banner_message: Option<String>,
    /// Banner type: "info", "warning", "error".
    pub banner_type: String,
}

impl InstanceSettings {
    pub fn is_banner_active(&self) -> bool {
        self.banner_message.is_some()
    }
}

impl From<rg_db::entities::instance_settings::Model> for InstanceSettings {
    fn from(row: rg_db::entities::instance_settings::Model) -> Self {
        Self {
            maintenance_mode: row.maintenance_mode,
            banner_message: row.banner_message,
            banner_type: row.banner_type,
        }
    }
}

/// One instance's view of its settings: the durable row, plus a memo of it.
///
/// Cheap to clone (it is a handle, not a copy) and scoped to the server state it
/// is built into, so two servers in one process never see each other's settings.
///
/// A single clone of this cache can be shared across transports in the same
/// process. That is how an admin's HTTP settings update reaches the SSH
/// receive-pack gate immediately instead of waiting for a restart.
#[derive(Clone, Default)]
pub struct InstanceSettingsCache {
    /// `None` = not yet read from the database.
    cached: Arc<RwLock<Option<InstanceSettings>>>,
}

impl InstanceSettingsCache {
    /// The current settings, reading the database once per process and then
    /// serving from memory.
    ///
    /// A database failure yields the defaults without caching them, so the next
    /// request retries rather than inheriting a wrong answer for the life of the
    /// process.
    pub async fn get(&self, db: &DatabaseConnection) -> InstanceSettings {
        if let Some(settings) = self.cached.read().await.as_ref() {
            return settings.clone();
        }

        let mut guard = self.cached.write().await;
        // Another task may have filled the cache while this one waited.
        if let Some(settings) = guard.as_ref() {
            return settings.clone();
        }

        match rg_db::ops::instance_settings_ops::find(db).await {
            Ok(row) => {
                let settings = row.map(InstanceSettings::from).unwrap_or_default();
                *guard = Some(settings.clone());
                settings
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read instance settings; using defaults");
                InstanceSettings::default()
            }
        }
    }

    /// Apply `f` to the current settings, persist the result, and refresh the
    /// cache - in that order, holding the write lock throughout so two
    /// concurrent updates cannot each build on the pre-update value.
    ///
    /// The cache is only advanced once the row is written: a failed write leaves
    /// both the database and the memo on the old value, and the caller learns the
    /// change did not take instead of watching it evaporate at the next restart.
    pub async fn update(
        &self,
        db: &DatabaseConnection,
        f: impl FnOnce(&mut InstanceSettings),
    ) -> Result<InstanceSettings, sea_orm::DbErr> {
        let mut guard = self.cached.write().await;
        let mut settings = match guard.as_ref() {
            Some(settings) => settings.clone(),
            None => rg_db::ops::instance_settings_ops::find(db)
                .await?
                .map(InstanceSettings::from)
                .unwrap_or_default(),
        };

        f(&mut settings);

        rg_db::ops::instance_settings_ops::save(
            db,
            settings.maintenance_mode,
            settings.banner_message.as_deref(),
            &settings.banner_type,
        )
        .await?;

        *guard = Some(settings.clone());
        Ok(settings)
    }
}
