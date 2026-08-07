//! Instance-wide settings (maintenance mode, banner).
//!
//! These describe one running instance, so they live in that instance's
//! database and are cached by the process state that serves it.

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// How long a settings read is served from memory before the row is consulted
/// again.
///
/// The settings row is a property of the *database*, not of the process holding
/// this memo, so on a fleet of instances sharing one database this constant is
/// the entire window in which `maintenance_mode` can be on for the instance that
/// accepted the PATCH and off for its peers. Without a bound the window is the
/// remaining lifetime of every other process — the gate exists to stop writes
/// before a backup or an upgrade, and an operator who flips it has no way to
/// know it took anywhere but on the one node they talked to.
///
/// Ten seconds rather than the 30 that `PERM_CACHE_TTL` allows: the cost is one
/// indexed single-row read per instance per ten seconds, and the operator's wait
/// after closing the gate is the thing being paid for.
const SETTINGS_TTL: Duration = Duration::from_secs(10);

/// One instance's view of its settings: the durable row, plus a memo of it that
/// expires after [`SETTINGS_TTL`].
///
/// Cheap to clone (it is a handle, not a copy) and scoped to the server state it
/// is built into, so two servers in one process never see each other's settings.
///
/// A single clone of this cache can be shared across transports in the same
/// process. That is how an admin's HTTP settings update reaches the SSH
/// receive-pack gate immediately instead of waiting for a restart. A *different*
/// process on the same database learns about it within `SETTINGS_TTL`.
#[derive(Clone)]
pub struct InstanceSettingsCache {
    /// `None` = never read from the database. `Some` carries the moment the
    /// value was read, which is what makes it expire.
    cached: Arc<RwLock<Option<(InstanceSettings, Instant)>>>,
    /// Normally [`SETTINGS_TTL`]; overridable so tests can observe a refresh
    /// without sleeping for the production window.
    ttl: Duration,
}

impl Default for InstanceSettingsCache {
    fn default() -> Self {
        Self {
            cached: Arc::default(),
            ttl: SETTINGS_TTL,
        }
    }
}

impl InstanceSettingsCache {
    /// A cache with a non-default refresh window.
    ///
    /// Production builds one via [`Default`]; this exists so a test can prove
    /// the refresh happens at all in less time than [`SETTINGS_TTL`].
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            cached: Arc::default(),
            ttl,
        }
    }

    /// The current settings, re-read from the database at most once per
    /// [`SETTINGS_TTL`] and served from memory in between.
    ///
    /// A database failure keeps serving the last known value rather than the
    /// defaults: `InstanceSettings::default()` says `maintenance_mode: false`,
    /// so falling back to it would report the gate open because the database is
    /// down — the one moment it should stay shut. The memo keeps its old
    /// timestamp, so the next call retries instead of inheriting the failure.
    pub async fn get(&self, db: &DatabaseConnection) -> InstanceSettings {
        if let Some((settings, read_at)) = self.cached.read().await.as_ref() {
            if read_at.elapsed() < self.ttl {
                return settings.clone();
            }
        }

        let mut guard = self.cached.write().await;
        // Another task may have refreshed the memo while this one waited.
        if let Some((settings, read_at)) = guard.as_ref() {
            if read_at.elapsed() < self.ttl {
                return settings.clone();
            }
        }

        match rg_db::ops::instance_settings_ops::find(db).await {
            Ok(row) => {
                let settings = row.map(InstanceSettings::from).unwrap_or_default();
                *guard = Some((settings.clone(), Instant::now()));
                settings
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to read instance settings; serving the last known values"
                );
                guard
                    .as_ref()
                    .map(|(settings, _)| settings.clone())
                    .unwrap_or_default()
            }
        }
    }

    /// Apply `f` to the current settings, persist the result, and refresh the
    /// cache - in that order, holding the write lock throughout so two
    /// concurrent updates cannot each build on the pre-update value.
    ///
    /// `f` is applied to the *durable row*, freshly read, never to the memo: on a
    /// fleet sharing one database the memo may be up to [`SETTINGS_TTL`] behind,
    /// and a read-modify-write based on it would silently roll back whatever a
    /// peer instance changed in that window. One extra row read per settings
    /// PATCH is not a cost worth arguing about.
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
        let mut settings = rg_db::ops::instance_settings_ops::find(db)
            .await?
            .map(InstanceSettings::from)
            .unwrap_or_default();

        f(&mut settings);

        rg_db::ops::instance_settings_ops::save(
            db,
            settings.maintenance_mode,
            settings.banner_message.as_deref(),
            &settings.banner_type,
        )
        .await?;

        *guard = Some((settings.clone(), Instant::now()));
        Ok(settings)
    }
}

/// card_90b36499e7bd: the settings memo used to be read from the database once
/// per process and never again, so `maintenance_mode` was on for exactly the
/// instance that accepted the PATCH and off for every peer sharing its database
/// until they were restarted.
#[cfg(test)]
mod instance_settings_cache_tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    /// The headline contract: two instances, one database. A flip on the first
    /// becomes visible to the second within the cache's named window — the whole
    /// point of the gate being that it stops writes on the *fleet*.
    #[tokio::test]
    async fn maintenance_flip_on_one_instance_reaches_another_within_the_ttl() {
        let db = setup_db().await;
        let ttl = Duration::from_millis(50);
        let instance_a = InstanceSettingsCache::with_ttl(ttl);
        let instance_b = InstanceSettingsCache::with_ttl(ttl);

        // Both instances have served a request, so both hold a warm memo.
        assert!(!instance_a.get(&db).await.maintenance_mode);
        assert!(!instance_b.get(&db).await.maintenance_mode);

        instance_a
            .update(&db, |s| s.maintenance_mode = true)
            .await
            .expect("write the settings row");
        assert!(
            instance_a.get(&db).await.maintenance_mode,
            "the instance that accepted the PATCH must see it at once"
        );

        tokio::time::sleep(ttl * 2).await;
        assert!(
            instance_b.get(&db).await.maintenance_mode,
            "a peer instance still lets writes through after the window closed"
        );

        // The reverse direction matters just as much: an instance left in
        // maintenance forever refuses every write on the node nobody flipped.
        instance_a
            .update(&db, |s| s.maintenance_mode = false)
            .await
            .expect("write the settings row");
        tokio::time::sleep(ttl * 2).await;
        assert!(
            !instance_b.get(&db).await.maintenance_mode,
            "a peer instance stayed in maintenance after it was lifted"
        );
    }

    /// Inside the window the memo is authoritative — that is what the TTL buys,
    /// and a test that passed whether or not the value was cached would not be
    /// pinning anything.
    #[tokio::test]
    async fn a_warm_memo_is_served_without_re_reading_within_the_window() {
        let db = setup_db().await;
        let instance_a = InstanceSettingsCache::default();
        let instance_b = InstanceSettingsCache::with_ttl(Duration::from_secs(600));

        assert!(!instance_b.get(&db).await.maintenance_mode);
        instance_a
            .update(&db, |s| s.maintenance_mode = true)
            .await
            .expect("write the settings row");

        assert!(
            !instance_b.get(&db).await.maintenance_mode,
            "the memo expired early, so the TTL above is not what bounds the window"
        );
    }

    /// A settings PATCH is a read-modify-write over a row a peer may have
    /// touched. Basing it on a stale memo silently reverts the peer's change.
    #[tokio::test]
    async fn an_update_does_not_roll_back_a_peer_change_it_has_not_seen_yet() {
        let db = setup_db().await;
        let instance_a = InstanceSettingsCache::with_ttl(Duration::from_secs(600));
        let instance_b = InstanceSettingsCache::with_ttl(Duration::from_secs(600));

        // A warms its memo, then B closes the gate.
        instance_a.get(&db).await;
        instance_b
            .update(&db, |s| s.maintenance_mode = true)
            .await
            .expect("write the settings row");

        // A now edits an unrelated field from its stale memo.
        let after = instance_a
            .update(&db, |s| s.banner_message = Some("upgrading".to_string()))
            .await
            .expect("write the settings row");

        assert!(
            after.maintenance_mode,
            "editing the banner re-opened the maintenance gate a peer had closed"
        );
        let row = rg_db::ops::instance_settings_ops::find(&db)
            .await
            .expect("read the settings row")
            .expect("the row exists after two saves");
        assert!(row.maintenance_mode, "the durable row lost the peer's flip");
        assert_eq!(row.banner_message.as_deref(), Some("upgrading"));
    }
}
