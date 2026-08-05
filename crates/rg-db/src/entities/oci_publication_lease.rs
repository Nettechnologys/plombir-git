//! OCI publication lease entity — maps to the `oci_publication_lease` table.
//!
//! One row exists only while some request is publishing the key it names, so
//! the table is empty on a quiet instance. See
//! `oci_ops::bid_for_publication_lease` for the protocol.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_publication_lease")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The backend key whose publication this lease covers.
    #[sea_orm(unique)]
    pub storage_key: String,
    /// Opaque identifier of the current holder.
    pub token: String,
    /// When the current holder took the lease — the input to expiry.
    pub since: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
