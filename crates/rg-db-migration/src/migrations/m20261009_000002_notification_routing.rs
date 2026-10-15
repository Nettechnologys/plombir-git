//! Notifications that reach the person an event is about (card_349c2b6a0d7c).
//!
//! Until now a notification went to a repository's watchers and nobody else,
//! for three events. A reviewer asked to approve a pull request, somebody
//! `@mentioned` in a comment, an assignee, the author of a pull request whose
//! CI just failed — none of them heard anything unless they happened to watch
//! the whole repository.
//!
//! Three pieces of schema carry the fix:
//!
//! * `notifications` learns what a row is *about* — `subject_type` /
//!   `subject_id` (an issue or a pull request), `reason` (why this person got
//!   it) and `link` (the page it opens). The subject is what lets a second
//!   event in the same thread update the unread row instead of stacking a new
//!   one, and what deleting an issue removes. `email_pending` marks a row the
//!   mail dispatcher still owes its recipient a message for; the dispatcher
//!   sends what piled up per person in one mail.
//! * `notification_settings` — one row per account that changed a default:
//!   which kinds of notification are also mailed. An account without a row has
//!   the defaults, so nothing is backfilled.
//! * `thread_subscriptions` — who follows an issue or a pull request. Taking
//!   part subscribes you; an explicit unsubscribe is a row with
//!   `subscribed = false`, so taking part again does not undo it.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261009_000002_notification_routing"
    }
}

const NOTIFICATIONS_BY_SUBJECT: &str = "idx_notifications_user_subject";
const NOTIFICATIONS_EMAIL_PENDING: &str = "idx_notifications_email_pending";
const UNIQUE_SUBSCRIPTION: &str = "uq_thread_subscriptions_user_subject";
const SUBSCRIPTIONS_BY_SUBJECT: &str = "idx_thread_subscriptions_subject";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            (
                "reason",
                ColumnDef::new(Notifications::Reason)
                    .string_len(32)
                    .null()
                    .to_owned(),
            ),
            (
                "subject_type",
                ColumnDef::new(Notifications::SubjectType)
                    .string_len(16)
                    .null()
                    .to_owned(),
            ),
            (
                "subject_id",
                ColumnDef::new(Notifications::SubjectId)
                    .big_integer()
                    .null()
                    .to_owned(),
            ),
            (
                "link",
                ColumnDef::new(Notifications::Link)
                    .string_len(512)
                    .null()
                    .to_owned(),
            ),
            (
                "updated_at",
                ColumnDef::new(Notifications::UpdatedAt)
                    .timestamp_with_time_zone()
                    .null()
                    .to_owned(),
            ),
            (
                "email_pending",
                ColumnDef::new(Notifications::EmailPending)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
        ];
        for (name, mut column) in columns {
            if !manager.has_column("notifications", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Notifications::Table)
                            .add_column(&mut column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        // MySQL has no `CREATE INDEX IF NOT EXISTS`: ask first, so a re-run
        // after a partial apply is a no-op instead of a duplicate-key error.
        if !manager
            .has_index("notifications", NOTIFICATIONS_BY_SUBJECT)
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name(NOTIFICATIONS_BY_SUBJECT)
                        .table(Notifications::Table)
                        .col(Notifications::UserId)
                        .col(Notifications::SubjectType)
                        .col(Notifications::SubjectId)
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_index("notifications", NOTIFICATIONS_EMAIL_PENDING)
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name(NOTIFICATIONS_EMAIL_PENDING)
                        .table(Notifications::Table)
                        .col(Notifications::EmailPending)
                        .col(Notifications::UserId)
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_table(
                Table::create()
                    .table(NotificationSettings::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NotificationSettings::UserId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::EmailReviewRequested)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::EmailMention)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::EmailAssigned)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::EmailCiFailed)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::EmailParticipating)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    // The mail every CI-triggering push used to send the
                    // repository owner unasked. Off unless chosen.
                    .col(
                        ColumnDef::new(NotificationSettings::EmailCiTriggered)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(NotificationSettings::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(NotificationSettings::Table, NotificationSettings::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ThreadSubscriptions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ThreadSubscriptions::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    // `issue` or `pull_request`.
                    .col(
                        ColumnDef::new(ThreadSubscriptions::SubjectType)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::SubjectId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::Subscribed)
                            .boolean()
                            .not_null(),
                    )
                    // Why the row exists: `author`, `commented`, `mention`,
                    // `assigned`, `review_requested`, or `manual`.
                    .col(
                        ColumnDef::new(ThreadSubscriptions::Reason)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ThreadSubscriptions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ThreadSubscriptions::Table, ThreadSubscriptions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ThreadSubscriptions::Table, ThreadSubscriptions::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        if !manager
            .has_index("thread_subscriptions", UNIQUE_SUBSCRIPTION)
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .unique()
                        .name(UNIQUE_SUBSCRIPTION)
                        .table(ThreadSubscriptions::Table)
                        .col(ThreadSubscriptions::UserId)
                        .col(ThreadSubscriptions::SubjectType)
                        .col(ThreadSubscriptions::SubjectId)
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_index("thread_subscriptions", SUBSCRIPTIONS_BY_SUBJECT)
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name(SUBSCRIPTIONS_BY_SUBJECT)
                        .table(ThreadSubscriptions::Table)
                        .col(ThreadSubscriptions::SubjectType)
                        .col(ThreadSubscriptions::SubjectId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ThreadSubscriptions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(NotificationSettings::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for index in [NOTIFICATIONS_BY_SUBJECT, NOTIFICATIONS_EMAIL_PENDING] {
            if manager.has_index("notifications", index).await? {
                manager
                    .drop_index(
                        Index::drop()
                            .name(index)
                            .table(Notifications::Table)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        for (name, column) in [
            ("email_pending", Notifications::EmailPending),
            ("updated_at", Notifications::UpdatedAt),
            ("link", Notifications::Link),
            ("subject_id", Notifications::SubjectId),
            ("subject_type", Notifications::SubjectType),
            ("reason", Notifications::Reason),
        ] {
            if manager.has_column("notifications", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Notifications::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Notifications {
    Table,
    UserId,
    Reason,
    SubjectType,
    SubjectId,
    Link,
    UpdatedAt,
    EmailPending,
}

#[derive(DeriveIden)]
enum NotificationSettings {
    Table,
    UserId,
    EmailReviewRequested,
    EmailMention,
    EmailAssigned,
    EmailCiFailed,
    EmailParticipating,
    EmailCiTriggered,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum ThreadSubscriptions {
    Table,
    Id,
    UserId,
    RepoId,
    SubjectType,
    SubjectId,
    Subscribed,
    Reason,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
