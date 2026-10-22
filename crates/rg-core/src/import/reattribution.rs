//! Giving imported history back to the person who wrote it (card_2a006cf365f6).
//!
//! An import writes everything as the account that ran it and keeps the source's
//! own record of the writer beside it: `original_author`, the source account id
//! in `original_author_id`, and the instance that issued that id in
//! `original_service` (card_04088376105b). A login is never matched to a local
//! account — the source's `alice` is not this instance's `alice`. What *can*
//! vouch for a source account is signing in with it: an `oauth_accounts` row is
//! the provider confirming that this local account holds that provider's
//! account id. So a person who has linked their GitHub sign-in may take back
//! what that GitHub account wrote.
//!
//! Three rules keep that from becoming a way to borrow somebody's history:
//!
//! * Only providers whose host is fixed vouch: the OAuth2 sign-ins named
//!   `github` and `gitlab` talk to github.com and gitlab.com, and their ids are
//!   matched against `original_service = "github"` / `"gitlab"` only. An import
//!   from any other server records `github:<host>`, which nothing here ever
//!   matches — user 42 on a GitHub Enterprise server is not user 42 on
//!   github.com.
//! * Nothing moves without the person asking: [`claimable`] only counts, and
//!   [`claim`] moves what the caller names, for repositories the caller has
//!   checked the person can read. The owner's decision was "with confirmation"
//!   — a change of author is not undone by anything.
//! * The byline stays. `original_*` still says where the row came from; only
//!   `author_id` (`reviewer_id` for a review) changes, and an imported verdict
//!   still authorizes nothing (`pr_review_ops::current_approvers`).

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, QueryFilter, QuerySelect,
    RelationTrait, TransactionTrait,
};

use rg_db::entities::{issue, issue_comment, pr_review, pull_request, release};

/// A source account this local account has signed in with, on an instance
/// whose ids can be trusted to name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// `github` (github.com) or `gitlab` (gitlab.com) — the only two
    /// `original_service` values a sign-in can vouch for.
    pub service: &'static str,
    /// The provider's account id, as `original_author_id` records it.
    pub account_id: i64,
    /// The login the provider reported at sign-in, for the confirmation screen.
    pub login: String,
}

/// What an account could take back in one repository, from one source account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimableRepository {
    pub repo_id: i64,
    pub service: &'static str,
    pub account_id: i64,
    pub issues: u64,
    pub comments: u64,
    pub pull_requests: u64,
    pub reviews: u64,
    pub releases: u64,
}

impl ClaimableRepository {
    pub fn total(&self) -> u64 {
        self.issues + self.comments + self.pull_requests + self.reviews + self.releases
    }
}

/// How many rows a [`claim`] moved, by kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClaimOutcome {
    pub issues: u64,
    pub comments: u64,
    pub pull_requests: u64,
    pub reviews: u64,
    pub releases: u64,
}

/// The source accounts `user_id` has signed in with through a provider whose
/// host is fixed.
///
/// A link to a provider the operator has since removed, or to one of another
/// type, vouches for nothing: without the provider row there is no knowing
/// which server issued the id. A provider user id that is not a number is not
/// one GitHub or GitLab issued.
pub async fn signed_in_identities(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<SourceIdentity>> {
    let links = rg_db::ops::oauth_account_ops::find_by_user_id(db, user_id)
        .await
        .context("failed to read the account's sign-in links")?;
    let providers = rg_db::ops::sso_provider_ops::list_all(db)
        .await
        .context("failed to read the sign-in providers")?;
    let mut identities = Vec::new();
    for link in links {
        let Some(provider) = providers.iter().find(|p| p.slug == link.provider) else {
            continue;
        };
        // How sign-in itself decides which server a provider is: an OAuth2
        // provider is the public one its slug names — `github` asks
        // api.github.com, `gitlab` asks gitlab.com
        // (`auth::sso::fetch_user_info_with_builtin_urls`) — while an OIDC
        // provider is whatever issuer its discovery URL names, whatever its
        // slug. Only the first kind's ids are github.com's or gitlab.com's.
        if provider.provider_type == "oidc" {
            continue;
        }
        let service = match provider.slug.as_str() {
            "github" => "github",
            "gitlab" => "gitlab",
            _ => continue,
        };
        let Ok(account_id) = link.provider_user_id.parse::<i64>() else {
            continue;
        };
        let identity = SourceIdentity {
            service,
            account_id,
            login: link.provider_username,
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
    }
    Ok(identities)
}

/// Everything imported under `identities` that is not `user_id`'s yet, counted
/// per repository and source account. Moves nothing.
///
/// The caller decides which of these repositories the person may be shown —
/// a count in a repository they cannot read says the repository exists.
pub async fn claimable(
    db: &DatabaseConnection,
    user_id: i64,
    identities: &[SourceIdentity],
) -> Result<Vec<ClaimableRepository>> {
    let mut rows: std::collections::BTreeMap<(i64, &'static str, i64), ClaimableRepository> =
        std::collections::BTreeMap::new();
    for identity in identities {
        let mut add = |repo_id: i64, count: i64, kind: fn(&mut ClaimableRepository) -> &mut u64| {
            let row = rows
                .entry((repo_id, identity.service, identity.account_id))
                .or_insert_with(|| ClaimableRepository {
                    repo_id,
                    service: identity.service,
                    account_id: identity.account_id,
                    ..ClaimableRepository::default()
                });
            *kind(row) += u64::try_from(count).unwrap_or(0);
        };

        for (repo_id, count) in issue::Entity::find()
            .select_only()
            .column(issue::Column::RepoId)
            .column_as(issue::Column::Id.count(), "count")
            .filter(issue::Column::OriginalService.eq(identity.service))
            .filter(issue::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(issue::Column::AuthorId.ne(user_id))
            .group_by(issue::Column::RepoId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await
            .context("failed to count claimable issues")?
        {
            add(repo_id, count, |row| &mut row.issues);
        }

        for (repo_id, count) in issue_comment::Entity::find()
            .select_only()
            .join(JoinType::InnerJoin, issue_comment::Relation::Issue.def())
            .column(issue::Column::RepoId)
            .column_as(issue_comment::Column::Id.count(), "count")
            .filter(issue_comment::Column::OriginalService.eq(identity.service))
            .filter(issue_comment::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(issue_comment::Column::AuthorId.ne(user_id))
            .group_by(issue::Column::RepoId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await
            .context("failed to count claimable comments")?
        {
            add(repo_id, count, |row| &mut row.comments);
        }

        for (repo_id, count) in pull_request::Entity::find()
            .select_only()
            .column(pull_request::Column::RepoId)
            .column_as(pull_request::Column::Id.count(), "count")
            .filter(pull_request::Column::OriginalService.eq(identity.service))
            .filter(pull_request::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(pull_request::Column::AuthorId.ne(user_id))
            .group_by(pull_request::Column::RepoId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await
            .context("failed to count claimable pull requests")?
        {
            add(repo_id, count, |row| &mut row.pull_requests);
        }

        for (repo_id, count) in pr_review::Entity::find()
            .select_only()
            .column(pr_review::Column::RepoId)
            .column_as(pr_review::Column::Id.count(), "count")
            .filter(pr_review::Column::OriginalService.eq(identity.service))
            .filter(pr_review::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(pr_review::Column::ReviewerId.ne(user_id))
            .group_by(pr_review::Column::RepoId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await
            .context("failed to count claimable reviews")?
        {
            add(repo_id, count, |row| &mut row.reviews);
        }

        // `author_id` is nullable here: a release whose importer account is
        // gone is still the source author's to claim.
        for (repo_id, count) in release::Entity::find()
            .select_only()
            .column(release::Column::RepoId)
            .column_as(release::Column::Id.count(), "count")
            .filter(release::Column::OriginalService.eq(identity.service))
            .filter(release::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(
                release::Column::AuthorId
                    .ne(user_id)
                    .or(release::Column::AuthorId.is_null()),
            )
            .group_by(release::Column::RepoId)
            .into_tuple::<(i64, i64)>()
            .all(db)
            .await
            .context("failed to count claimable releases")?
        {
            add(repo_id, count, |row| &mut row.releases);
        }
    }
    Ok(rows.into_values().collect())
}

/// Make `user_id` the author of everything imported under `identities` in
/// `repo_ids`, in one transaction.
///
/// `repo_ids` is what the person confirmed, already narrowed by the caller to
/// repositories they can read. Rows of other repositories, and rows that are
/// already theirs, are left exactly as they are.
pub async fn claim(
    db: &DatabaseConnection,
    user_id: i64,
    identities: &[SourceIdentity],
    repo_ids: &[i64],
) -> Result<ClaimOutcome> {
    let mut outcome = ClaimOutcome::default();
    if identities.is_empty() || repo_ids.is_empty() {
        return Ok(outcome);
    }
    let txn = db
        .begin()
        .await
        .context("failed to begin the re-attribution")?;
    for identity in identities {
        outcome.issues += issue::Entity::update_many()
            .col_expr(issue::Column::AuthorId, Expr::value(user_id))
            .filter(issue::Column::OriginalService.eq(identity.service))
            .filter(issue::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(issue::Column::AuthorId.ne(user_id))
            .filter(issue::Column::RepoId.is_in(repo_ids.iter().copied()))
            .exec(&txn)
            .await
            .context("failed to re-attribute issues")?
            .rows_affected;

        outcome.comments += issue_comment::Entity::update_many()
            .col_expr(issue_comment::Column::AuthorId, Expr::value(user_id))
            .filter(issue_comment::Column::OriginalService.eq(identity.service))
            .filter(issue_comment::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(issue_comment::Column::AuthorId.ne(user_id))
            .filter(
                issue_comment::Column::IssueId.in_subquery(
                    Query::select()
                        .column(issue::Column::Id)
                        .from(issue::Entity)
                        .and_where(issue::Column::RepoId.is_in(repo_ids.iter().copied()))
                        .to_owned(),
                ),
            )
            .exec(&txn)
            .await
            .context("failed to re-attribute comments")?
            .rows_affected;

        outcome.pull_requests += pull_request::Entity::update_many()
            .col_expr(pull_request::Column::AuthorId, Expr::value(user_id))
            .filter(pull_request::Column::OriginalService.eq(identity.service))
            .filter(pull_request::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(pull_request::Column::AuthorId.ne(user_id))
            .filter(pull_request::Column::RepoId.is_in(repo_ids.iter().copied()))
            .exec(&txn)
            .await
            .context("failed to re-attribute pull requests")?
            .rows_affected;

        outcome.reviews += pr_review::Entity::update_many()
            .col_expr(pr_review::Column::ReviewerId, Expr::value(user_id))
            .filter(pr_review::Column::OriginalService.eq(identity.service))
            .filter(pr_review::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(pr_review::Column::ReviewerId.ne(user_id))
            .filter(pr_review::Column::RepoId.is_in(repo_ids.iter().copied()))
            .exec(&txn)
            .await
            .context("failed to re-attribute reviews")?
            .rows_affected;

        outcome.releases += release::Entity::update_many()
            .col_expr(release::Column::AuthorId, Expr::value(user_id))
            .filter(release::Column::OriginalService.eq(identity.service))
            .filter(release::Column::OriginalAuthorId.eq(identity.account_id))
            .filter(
                release::Column::AuthorId
                    .ne(user_id)
                    .or(release::Column::AuthorId.is_null()),
            )
            .filter(release::Column::RepoId.is_in(repo_ids.iter().copied()))
            .exec(&txn)
            .await
            .context("failed to re-attribute releases")?
            .rows_affected;
    }
    txn.commit()
        .await
        .context("failed to commit the re-attribution")?;
    Ok(outcome)
}
