//! The one place an `audit_log` row is built.
//!
//! This module is deliberately the only writer in the workspace, and
//! `audit_writer_guard` in `rg-http` is what keeps it that way. The reason is
//! not tidiness. There used to be four byte-identical copies of `record_audit`
//! in `crates/rg-http/src/api/{admin,orgs,users,repos}.rs`, and they did not
//! drift in *mechanics* — they drifted in *meaning*. The actor column ended up
//! holding four different kinds of value: a real username, the numeric
//! `claims.sub`, an empty string, and — in `orgs.rs`, with a comment saying so —
//! the name of the **organization being acted on**. `/admin/audit` renders that
//! column as `{username} (#{user_id})`, so one list showed `alice (#7)`,
//! `7 (#7)`, a blank actor, and `acme-corp (#3)`, where `acme-corp` is not who
//! did anything (card_fcc07f8d1505, card_51f6f3a99003).
//!
//! ## Why the actor is a type
//!
//! Collapsing the copies alone would not have fixed that: a single writer taking
//! `username: &str` accepts an org name just as happily as four writers did. So
//! the actor is [`AuditActor`], it has no constructor that takes a name, and the
//! only way to get one carrying a name is to look the account up by id. A caller
//! *cannot* put an organization, an id, or an empty string in the actor column;
//! the mistake is no longer available.
//!
//! ## Why the name comes from the row and not the session
//!
//! A session minted before a rename still carries the old spelling in its
//! claims, and this column is what a human reads months later. `create_repo`
//! already resolved the actor from the account row and said so in a comment;
//! that rule is now the only one there is.
//!
//! ## Failure is never an empty string
//!
//! A lookup that failed is not "there is no actor". The two resolvers below
//! differ only in what they do about that, and the choice is a real one — see
//! [`AuditActor::resolve`] versus [`AuditActor::resolve_after_the_fact`].

use sea_orm::{DatabaseConnection, EntityTrait, NotSet, Set};

use rg_db::entities::audit_log;

/// Who performed an audited action.
///
/// Holds an account id and, when it could be read, that account's name. There is
/// no constructor taking a name: the name is always read from the account row,
/// which is what makes "the actor column contains the actor" a property of the
/// type rather than a convention four modules each had to remember.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditActor {
    id: Option<i64>,
    name: Option<String>,
}

impl AuditActor {
    /// The name this actor will be written under, when it has one.
    ///
    /// For the call sites where the actor *is* the resource — an account adding
    /// a token or an SSH key to itself — so that the resource name comes from
    /// the same lookup the actor column does instead of a second query that
    /// could disagree with it. There is deliberately no setter: the value can
    /// only ever have come from [`AuditActor::resolve`].
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// An action no account performed — a scheduler pass, a system sweep.
    ///
    /// Both columns stay `NULL`, which is the honest encoding and the one the
    /// API renders as `null`. An empty string would render as a blank actor and
    /// be indistinguishable from a name that failed to load.
    pub fn none() -> Self {
        Self::default()
    }

    /// Resolve the acting account's name, refusing to guess.
    ///
    /// For the call sites that name the actor **before** performing the
    /// mutation. A `Err` here is a database failure, not an absent actor, and it
    /// propagates so the handler can answer `5xx` without having acted:
    /// `admin.unlock_user` used to write `.ok().flatten().unwrap_or_default()`,
    /// which folded "the query failed" and "no such user" into `""` and then
    /// went on to reset the target's login failures anyway — leaving the one
    /// record of who unlocked an account with a blank author, looking entirely
    /// routine (card_86f40189bc71, card_e2bd7026c87d).
    ///
    /// A missing row is an error here too, and for a different reason: the id
    /// comes from a gate that already resolved it against something this account
    /// owns, so the account has to exist. That is the server's inconsistency,
    /// not the caller's mistake — which is what `repo.delete` and
    /// `repo.transfer` already answered `500` for, and this keeps that.
    pub async fn resolve(db: &DatabaseConnection, user_id: i64) -> anyhow::Result<Self> {
        let name = rg_db::ops::user_ops::find_by_id(db, user_id)
            .await?
            .map(|user| user.username)
            .ok_or_else(|| anyhow::anyhow!("authenticated actor {user_id} has no account row"))?;
        Ok(Self {
            id: Some(user_id),
            name: Some(name),
        })
    }

    /// Resolve the actor for an action that has **already happened**.
    ///
    /// The mutation is done and its result is owed to the caller, so neither a
    /// failed name lookup nor a vanished row may turn a completed login or
    /// registration into a `5xx`. What they must also not do is invent a name:
    /// the row is written with the id and no name, and the reason goes to the
    /// log — the one place it can still be recovered from.
    pub async fn resolve_after_the_fact(db: &DatabaseConnection, user_id: i64) -> Self {
        match Self::resolve(db, user_id).await {
            Ok(actor) => actor,
            Err(error) => {
                tracing::warn!(
                    user_id,
                    error = %format!("{error:#}"),
                    "the actor's name could not be read for an action that already happened; the \
                     audit row keeps the id and records no name rather than a blank one"
                );
                Self {
                    id: Some(user_id),
                    name: None,
                }
            }
        }
    }

    /// The account id, for callers that need to record it elsewhere too.
    pub fn id(&self) -> Option<i64> {
        self.id
    }
}

/// Record an audit event. Never fails the caller.
///
/// Fire-and-forget by design — an audit write that could fail a request would
/// make the journal a liability rather than a record. The warning below is
/// therefore the *only* trace such an event leaves, so it names what the event
/// was about rather than merely that a write failed.
#[tracing::instrument(skip(db, headers, details), fields(action = %action))]
#[allow(clippy::too_many_arguments)]
pub async fn record(
    db: &DatabaseConnection,
    actor: &AuditActor,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<i64>,
    resource_name: Option<&str>,
    headers: Option<&http::HeaderMap>,
    details: Option<serde_json::Value>,
) {
    let (ip_address, user_agent) = headers.map(extract_ip_and_ua).unwrap_or((None, None));
    let details = with_credential(details, crate::auth::credential_context::current());

    let entry = audit_log::ActiveModel {
        id: NotSet,
        user_id: Set(actor.id),
        username: Set(actor.name.clone()),
        action: Set(action.to_string()),
        resource_type: Set(resource_type.map(str::to_string)),
        resource_id: Set(resource_id),
        resource_name: Set(resource_name.map(str::to_string)),
        ip_address: Set(ip_address),
        user_agent: Set(user_agent),
        details: Set(details.map(|value| value.to_string())),
        created_at: Set(chrono::Utc::now()),
    };

    if let Err(error) = audit_log::Entity::insert(entry).exec(db).await {
        tracing::warn!(
            %action,
            user_id = ?actor.id,
            resource_type = ?resource_type,
            resource_id = ?resource_id,
            resource_name = ?resource_name,
            error = %format!("{error:#}"),
            "audit event not written — this action leaves no trace in the audit log"
        );
    }
}

/// Stamp the credential the request came through onto an audit row's details.
///
/// An action an agent took through a Personal Access Token — and, on this
/// instance's MCP endpoint, through which tool — is otherwise indistinguishable
/// from the same action taken in the browser: the actor column names the
/// account and nothing else. The stamp lives under one key, `credential`, so it
/// can never overwrite a field the caller wrote.
fn with_credential(
    details: Option<serde_json::Value>,
    credential: Option<crate::auth::credential_context::CredentialContext>,
) -> Option<serde_json::Value> {
    let Some(credential) = credential.filter(|c| c.token_id.is_some() || c.mcp_tool.is_some())
    else {
        return details;
    };
    let mut stamp = serde_json::Map::new();
    if let Some(token_id) = credential.token_id {
        stamp.insert("token_id".to_string(), token_id.into());
    }
    if let Some(tool) = credential.mcp_tool {
        stamp.insert("mcp_tool".to_string(), tool.into());
    }
    let mut object = match details {
        Some(serde_json::Value::Object(object)) => object,
        None => serde_json::Map::new(),
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("value".to_string(), other);
            object
        }
    };
    object.insert("credential".to_string(), serde_json::Value::Object(stamp));
    Some(serde_json::Value::Object(object))
}

/// Client IP and User-Agent, as far as either can be trusted.
///
/// The forwarded address is parsed as an `IpAddr` rather than copied through: an
/// unparsed `X-Forwarded-For` is attacker-controlled text landing in a column an
/// operator reads as an address. The User-Agent is cut at 512 characters for the
/// same reason — it is a header, and its length is the client's choice.
pub fn extract_ip_and_ua(headers: &http::HeaderMap) -> (Option<String>, Option<String>) {
    let ip_address = headers
        .get("X-Forwarded-For")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.trim().parse::<std::net::IpAddr>().ok())
        .map(|address| address.to_string())
        .or_else(|| {
            headers
                .get("X-Real-IP")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<std::net::IpAddr>().ok())
                .map(|address| address.to_string())
        });

    let user_agent = headers
        .get(http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(512).collect());

    (ip_address, user_agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    /// A database with every table, and one with none — the second is how a
    /// failing lookup is produced without a mock backend: `users` simply is not
    /// there, so `find_by_id` returns a real `DbErr` rather than `Ok(None)`.
    async fn db(migrated: bool) -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        if migrated {
            rg_db::run_migrations(&db).await.expect("run migrations");
        }
        db
    }

    /// The distinction card_86f40189bc71 and card_e2bd7026c87d are both about:
    /// a lookup that *failed* is not an actor that is *absent*, and neither of
    /// them is an empty name.
    ///
    /// `admin.unlock_user` used to collapse all three into `""` with
    /// `.ok().flatten().unwrap_or_default()` and then reset the target's login
    /// failures anyway — so the only record of who unlocked an account carried a
    /// blank author and looked entirely routine.
    #[tokio::test]
    async fn a_failed_lookup_is_refused_and_never_becomes_a_blank_name() {
        let broken = db(false).await;

        let refused = AuditActor::resolve(&broken, 7)
            .await
            .expect_err("a database failure must not pass for an actor");
        assert!(
            format!("{refused:#}").to_lowercase().contains("users"),
            "the refusal has to name what could not be read: {refused:#}"
        );

        // The after-the-fact resolver cannot refuse — the action already
        // happened — but it must degrade to "id, no name", never to a name.
        let degraded = AuditActor::resolve_after_the_fact(&broken, 7).await;
        assert_eq!(degraded.id(), Some(7), "the identity is not lost");
        assert_eq!(
            degraded.name, None,
            "a failed lookup invented a name instead of recording none"
        );
        assert_ne!(
            degraded.name.as_deref(),
            Some(""),
            "an empty name is what the journal cannot tell from a missing one"
        );
    }

    /// An actor id with no row is the server's inconsistency, not an anonymous
    /// action: every caller of `resolve` got that id from a gate that had
    /// already resolved it against something the account owns.
    #[tokio::test]
    async fn an_actor_id_with_no_account_row_is_refused_rather_than_left_nameless() {
        let empty = db(true).await;
        let refused = AuditActor::resolve(&empty, 7)
            .await
            .expect_err("an id with no account row must not pass for an actor");
        assert!(
            format!("{refused:#}").contains("no account row"),
            "{refused:#}"
        );
    }

    /// The encoding the whole phase turns on: no actor is `NULL`, not `""`.
    /// `/admin/audit` renders a blank string as a blank actor, which reads like
    /// a name that failed to load rather than an action nobody performed.
    #[test]
    fn an_action_with_no_account_carries_no_name_rather_than_an_empty_one() {
        let actor = AuditActor::none();
        assert_eq!(actor.id(), None);
        assert_eq!(
            actor.name, None,
            "an absent actor must not be an empty name"
        );
    }

    /// A header the client wrote is not an address until it parses as one.
    #[test]
    fn a_forwarded_address_that_is_not_an_address_is_not_recorded() {
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "not-an-ip".parse().unwrap());
        assert_eq!(extract_ip_and_ua(&headers).0, None);

        headers.insert("X-Forwarded-For", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(
            extract_ip_and_ua(&headers).0.as_deref(),
            Some("203.0.113.7"),
            "the client-facing hop is the first entry"
        );
    }

    /// A row written while a token is the request's credential says which one,
    /// and through which MCP tool — under its own key, never over a caller's.
    #[test]
    fn the_credential_is_stamped_under_its_own_key() {
        use crate::auth::credential_context::CredentialContext;
        let credential = CredentialContext {
            user_id: 7,
            token_id: Some(11),
            mcp_tool: Some("create_pr".to_string()),
            deny_protected_writes: false,
        };

        let stamped = with_credential(
            Some(serde_json::json!({"number": 3, "credential": "caller's"})),
            Some(credential.clone()),
        )
        .unwrap();
        assert_eq!(stamped["number"], 3);
        assert_eq!(stamped["credential"]["token_id"], 11);
        assert_eq!(stamped["credential"]["mcp_tool"], "create_pr");

        let wrapped = with_credential(Some(serde_json::json!("text")), Some(credential)).unwrap();
        assert_eq!(wrapped["value"], "text");
        assert_eq!(wrapped["credential"]["token_id"], 11);

        // A browser session publishes no token: the row stays as written.
        let session = CredentialContext {
            user_id: 7,
            ..Default::default()
        };
        assert_eq!(with_credential(None, Some(session)), None);
        assert_eq!(with_credential(None, None), None);
    }

    /// The User-Agent's length is the client's choice, so the column's is ours.
    #[test]
    fn an_unbounded_user_agent_is_cut_to_a_bounded_one() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            "a".repeat(4096).parse().expect("a long header value"),
        );
        assert_eq!(extract_ip_and_ua(&headers).1.map(|ua| ua.len()), Some(512));
    }
}
