//! card_e9b4da7bf8ca: drop the OCI blob reference count nothing consumes.
//!
//! `oci_blob.ref_count` was half of a garbage collector that was never built.
//! Nothing read the column, `decrement_blob_ref` had no callers, and the
//! registry exposes no `DELETE` at all — no manifest delete, no tag delete, no
//! blob delete — so a reference could never be released and the number could
//! only grow. It was not even a reference count: the tag path incremented on
//! every push of the same tag, so re-pushing `latest` twice recorded two
//! references to one manifest's layers.
//!
//! Keeping it would have been worse than useless. The entity documented it as
//! "blob is garbage-collected when this reaches 0", and a collector built on
//! that number would have deleted live layers — a freshly mounted blob sits at
//! `0` until a manifest claims it.
//!
//! What the writes were really buying is kept: the manifest transaction still
//! proves every referenced blob exists in the repository before it commits,
//! now as an explicit claim on both manifest paths rather than as a side effect
//! of bumping a counter (`oci_ops::claim_referenced_blobs`).
//!
//! `down` restores the column with its `0` default. The counts it held were
//! not reconstructible from anything else and were not read by anything, so
//! reversing the migration restores the shape, not the numbers.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260808_000001_drop_oci_blob_ref_count"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("oci_blob", "ref_count").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OciBlob::Table)
                        .drop_column(OciBlob::RefCount)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("oci_blob", "ref_count").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OciBlob::Table)
                        .add_column(
                            ColumnDef::new(OciBlob::RefCount)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OciBlob {
    Table,
    RefCount,
}
