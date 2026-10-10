//! The read and write connections carried across service boundaries.

use sea_orm::DatabaseConnection;

/// A pool pair for services that may queue background writes. On backends
/// without a dedicated SQLite writer, both fields may name the same pool.
#[derive(Clone)]
pub struct Db {
    read: DatabaseConnection,
    write: DatabaseConnection,
}

impl Db {
    pub fn new(read: DatabaseConnection, write: DatabaseConnection) -> Self {
        Self { read, write }
    }
}

/// A single connection remains useful for tests and callers without a
/// separately configured writer. Server entry points pass [`Db`] explicitly.
pub trait DbPools {
    fn read(&self) -> &DatabaseConnection;
    fn write(&self) -> &DatabaseConnection;

    fn owned(&self) -> Db {
        Db::new(self.read().clone(), self.write().clone())
    }
}

impl DbPools for Db {
    fn read(&self) -> &DatabaseConnection {
        &self.read
    }

    fn write(&self) -> &DatabaseConnection {
        &self.write
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
