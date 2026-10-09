//! Aggregates whose result type differs between the three backends.
//!
//! `SUM` over an integer column is not an integer everywhere: SQLite answers
//! `INTEGER`, PostgreSQL widens `sum(bigint)` to `numeric`, and MySQL answers
//! `DECIMAL` for any exact-integer `SUM`. sqlx type-checks a non-NULL value
//! before decoding it, so `Option<i64>` refuses the PostgreSQL and MySQL
//! answer with "mismatched types" — but only once the table has a row, since
//! the `SUM` of nothing is `NULL` and skips the check. SQLite tests stay green
//! and the first real upload on a server database turns the query into a 500.

use sea_orm::sea_query::{Alias, Expr, Func, SimpleExpr};
use sea_orm::{ColumnTrait, DbBackend};

/// `SUM(column)` as a value every backend decodes into `Option<i64>`.
///
/// PostgreSQL and MySQL get an explicit cast back to a 64-bit integer, spelled
/// the way each one accepts it (`BIGINT` is not a `CAST` target on MySQL,
/// `SIGNED` is its 64-bit spelling). A total beyond `i64::MAX` makes the cast
/// fail loudly instead of wrapping, which no byte or minute counter here can
/// reach.
pub(crate) fn sum_i64(backend: DbBackend, column: impl ColumnTrait) -> SimpleExpr {
    let sum: SimpleExpr = Func::sum(Expr::col(column)).into();
    match backend {
        DbBackend::Postgres => Func::cast_as(sum, Alias::new("BIGINT")).into(),
        DbBackend::MySql => Func::cast_as(sum, Alias::new("SIGNED")).into(),
        DbBackend::Sqlite => sum,
    }
}

#[cfg(test)]
mod tests {
    use super::sum_i64;
    use crate::entities::lfs_object;
    use sea_orm::{DbBackend, EntityTrait, QuerySelect, QueryTrait};

    fn rendered(backend: DbBackend) -> String {
        lfs_object::Entity::find()
            .select_only()
            .column_as(sum_i64(backend, lfs_object::Column::Size), "bytes")
            .build(backend)
            .to_string()
    }

    #[test]
    fn each_backend_gets_the_cast_it_accepts() {
        assert!(
            rendered(DbBackend::Postgres).contains(r#"CAST(SUM("size") AS BIGINT)"#),
            "{}",
            rendered(DbBackend::Postgres)
        );
        assert!(
            rendered(DbBackend::MySql).contains("CAST(SUM(`size`) AS SIGNED)"),
            "{}",
            rendered(DbBackend::MySql)
        );
        let sqlite = rendered(DbBackend::Sqlite);
        assert!(sqlite.contains(r#"SUM("size")"#), "{sqlite}");
        assert!(!sqlite.contains("CAST"), "{sqlite}");
    }
}
