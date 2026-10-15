//! The columns of the user-grant tables that the grant data migrations and
//! [`crate::serialized_user_grants`] read and write — and no others.
//!
//! These used to be `rg-db`'s live entities, which tied a migration dated
//! 2026-08-06 to whatever those entities say today, and tied the migration
//! crate to `rg-db`. Each module declares only the columns that code touches
//! (`select_only` reads, `update_many` writes, two-column grant inserts), so a
//! column added to a live table later does not reach them.
//!
//! `rg-db`'s `grant_tables_match_live_entities` test asserts that every column
//! here still exists on the live entity of the same table, so a rename there
//! goes red at test time rather than at the first account deletion.

macro_rules! empty_relation {
    () => {
        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    };
}

pub mod user {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub is_active: bool,
        pub deleted_at: Option<DateTimeUtc>,
    }

    empty_relation!();
}

pub mod protected_branch {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "protected_branches")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub allowed_push_user_ids: Option<String>,
        pub updated_at: DateTimeUtc,
    }

    empty_relation!();
}

pub mod protected_tag {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "protected_tags")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub allowed_user_ids: Option<String>,
        pub updated_at: DateTimeUtc,
    }

    empty_relation!();
}

pub mod ci_environment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "ci_environments")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub allowed_approver_ids: Option<String>,
        pub updated_at: DateTimeUtc,
    }

    empty_relation!();
}

pub mod protected_branch_push_grant {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "protected_branch_push_grants")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub protected_branch_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: i64,
    }

    empty_relation!();
}

pub mod protected_tag_push_grant {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "protected_tag_push_grants")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub protected_tag_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: i64,
    }

    empty_relation!();
}

pub mod ci_environment_approver_grant {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "ci_environment_approver_grants")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub environment_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: i64,
    }

    empty_relation!();
}
