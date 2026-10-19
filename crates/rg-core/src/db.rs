//! The read and write connections carried across service boundaries.

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::push_hooks::PushNotifier;

/// A pool pair for services that may queue background writes. On backends
/// without a dedicated SQLite writer, both fields may name the same pool.
#[derive(Clone)]
pub struct Db {
    read: DatabaseConnection,
    write: DatabaseConnection,
    /// The real-time sink for the notification rows written through these
    /// pools. It rides here because the rows are written deep inside
    /// `issue::service`, `review::service` and CI, which the pools reach and
    /// the HTTP layer's hub never did: until card_6abf2e3c4287 a requested
    /// review, a mention or a failed pipeline landed in the inbox and reached
    /// no open socket. Not a process global — a test process runs many apps,
    /// each with a hub of its own.
    notifier: Option<Arc<dyn PushNotifier>>,
}

impl Db {
    pub fn new(read: DatabaseConnection, write: DatabaseConnection) -> Self {
        Self {
            read,
            write,
            notifier: None,
        }
    }

    /// These pools, telling `notifier` about every notification row written
    /// through them. `None` leaves them silent.
    pub fn with_notifier(mut self, notifier: Option<Arc<dyn PushNotifier>>) -> Self {
        self.notifier = notifier;
        self
    }
}

/// A single connection remains useful for tests and callers without a
/// separately configured writer. Server entry points pass [`Db`] explicitly.
pub trait DbPools {
    fn read(&self) -> &DatabaseConnection;
    fn write(&self) -> &DatabaseConnection;

    /// Where notification rows written through these pools are announced in
    /// real time. A bare connection has no hub.
    fn notifier(&self) -> Option<&Arc<dyn PushNotifier>> {
        None
    }

    fn owned(&self) -> Db {
        Db::new(self.read().clone(), self.write().clone()).with_notifier(self.notifier().cloned())
    }
}

impl DbPools for Db {
    fn read(&self) -> &DatabaseConnection {
        &self.read
    }

    fn write(&self) -> &DatabaseConnection {
        &self.write
    }

    fn notifier(&self) -> Option<&Arc<dyn PushNotifier>> {
        self.notifier.as_ref()
    }
}

impl DbPools for DatabaseConnection {
    fn read(&self) -> &DatabaseConnection {
        self
    }

    fn write(&self) -> &DatabaseConnection {
        self
    }
}
