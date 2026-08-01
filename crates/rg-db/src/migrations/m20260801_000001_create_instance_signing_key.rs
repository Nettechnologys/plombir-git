//! Migration: create `instance_signing_key` — the durable home of this
//! instance's Ed25519 provenance identity.
//!
//! Until card_3aecf3708ebe the key was *derived* from `[auth].jwt_secret`, so
//! rotating the signing secret — the thing an operator is told to do the moment
//! a token leaks — silently replaced the instance's public identity. Every DSSE
//! envelope already stored in `release_assets.attestation` stopped verifying,
//! and the `kid` published at `/api/v1/ci/oidc/jwks` changed under every
//! external verifier that had fetched it. A signature the server itself issued
//! came back "invalid" from the server itself.
//!
//! A key that outlives a config value has to live where the data lives, so it
//! gets a row. One row, always id 1, for the same reason `instance_settings`
//! has one: an instance has exactly one identity, and a second row would be a
//! second answer to a question that has one.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(InstanceSigningKey::Table)
                    .if_not_exists()
                    // Not auto-increment: the writer always names id 1, which is
                    // what keeps the singleton a singleton.
                    .col(
                        ColumnDef::new(InstanceSigningKey::Id)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    // The 32-byte Ed25519 seed, hex-encoded and then AES-GCM
                    // encrypted under `[auth].encryption_key` — the same
                    // treatment every other at-rest secret gets. A database
                    // dump must not hand over the private half of a key whose
                    // public half external verifiers trust.
                    .col(
                        ColumnDef::new(InstanceSigningKey::SeedEncrypted)
                            .text()
                            .not_null(),
                    )
                    // When this instance first established an identity.
                    .col(
                        ColumnDef::new(InstanceSigningKey::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // `NULL` until the key is deliberately replaced — the one
                    // event that invalidates already-published attestations, so
                    // it is worth being able to date afterwards.
                    .col(
                        ColumnDef::new(InstanceSigningKey::RotatedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(InstanceSigningKey::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum InstanceSigningKey {
    Table,
    Id,
    SeedEncrypted,
    CreatedAt,
    RotatedAt,
}
