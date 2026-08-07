//! Migration: create `webauthn_ceremony_spend` — the WebAuthn ceremonies whose
//! challenge has already been answered.
//!
//! A WebAuthn challenge is single-use by design: the relying party issues a
//! nonce for one ceremony, and the assertion signed over it may be accepted
//! once. ForgeKeep kept no state at all for it. The in-progress ceremony lives
//! in a signed, short-lived cookie (`rg_core::auth::webauthn::seal_state`), and
//! a signature plus an `exp` answer only "did we issue this, and is it still
//! young" — never "has it already been used". So one intercepted
//! `POST /users/passkeys/login/finish` — the cookie and the assertion body,
//! exactly the pair a phishing proxy sees — could be replayed for the whole
//! 300-second lifetime of the cookie, and each replay was answered a session
//! (`card_7c7ada2d6a72`).
//!
//! The signature counter compare-and-swap
//! (`m20260803_000002_add_passkey_credential_rp_id`'s neighbour,
//! `passkey_credential_ops::touch_and_update`) closes only the half where the
//! counter *moved*. Most platform passkeys — iCloud Keychain and the like —
//! always report `signCount = 0`, so a replay re-stores a byte-identical
//! credential blob, the write honestly reports `Stored`, and the login
//! completes. On the most common authenticator there is, the replay went
//! through.
//!
//! No existing row can hold the spend. `passkey_credentials` does not exist yet
//! during registration, and a "last challenge" column on either that table or
//! `users` only refuses the *immediately* repeated challenge: replay an older
//! ceremony after a newer honest one has landed and the columns no longer
//! disagree. What has to be recorded is the set of challenges spent inside the
//! window, which is what this table is.
//!
//! It is deliberately not a session store: nothing about the ceremony is kept
//! here, only that a ceremony id was answered, and only until the cookie
//! carrying it can no longer be unsealed. `webauthn_ceremony_ops::spend` drops
//! expired rows on the way past, so the table holds at most the ceremonies of
//! the last few minutes and needs no background sweep to stay bounded — a sweep
//! nothing calls is how `password_reset_token_ops::delete_expired` ended up
//! being a comment (`card_dc5ea612d97a`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(WebauthnCeremonySpend::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WebauthnCeremonySpend::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The server-issued identifier of one ceremony, sealed into
                    // the cookie beside its challenge. UNIQUE because it *is*
                    // the thing being arbitrated: the constraint is what makes
                    // two concurrent replays of one assertion resolve to one
                    // winner inside the database rather than in two racing
                    // handlers.
                    //
                    // No foreign key to `users`: the row must outlive an account
                    // deleted mid-ceremony, since a spend that disappears is a
                    // replay window that reopens.
                    .col(
                        ColumnDef::new(WebauthnCeremonySpend::CeremonyId)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    // When the ceremony was answered. Not read by the gate —
                    // kept because "when was this challenge spent" is the first
                    // question asked of a replay that shows up in the logs.
                    .col(
                        ColumnDef::new(WebauthnCeremonySpend::SpentAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // When the record may be dropped: the instant after which
                    // the cookie carrying this ceremony can no longer be
                    // unsealed at all, so forgetting the spend can no longer
                    // let anything through.
                    .col(
                        ColumnDef::new(WebauthnCeremonySpend::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webauthn_ceremony_spend_expires_at")
                    .table(WebauthnCeremonySpend::Table)
                    .col(WebauthnCeremonySpend::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WebauthnCeremonySpend::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum WebauthnCeremonySpend {
    Table,
    Id,
    CeremonyId,
    SpentAt,
    ExpiresAt,
}
