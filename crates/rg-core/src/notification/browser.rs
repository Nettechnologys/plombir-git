//! A signed browser witness conveys no login authority. Only a caller which
//! has completed authentication may record it for an account.
use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rg_db::{
    entities::known_browser, notification::Event, ops::notification_delivery_ops as queue,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set, TransactionTrait};
use sha2::Sha256;

pub const COOKIE_NAME: &str = "plombir_browser_witness";
pub const LIFETIME_SECONDS: i64 = 180 * 24 * 60 * 60;
const DOMAIN: &[u8] = b"plombir-git/browser-witness/signing-key/v1";

#[derive(Clone)]
pub struct Witness {
    value: String,
    nonce: String,
    expires_at: i64,
}

fn mac(secret: &str) -> Hmac<Sha256> {
    let mut derive =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    derive.update(DOMAIN);
    Hmac::<Sha256>::new_from_slice(&derive.finalize().into_bytes()).expect("derived HMAC key")
}

impl Witness {
    pub fn issue(secret: &str, now: i64) -> Self {
        let mut nonce = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let nonce = URL_SAFE_NO_PAD.encode(nonce);
        let expires_at = now + LIFETIME_SECONDS;
        let payload = format!("v1.{expires_at}.{nonce}");
        let mut signer = mac(secret);
        signer.update(payload.as_bytes());
        let value = format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(signer.finalize().into_bytes())
        );
        Self {
            value,
            nonce,
            expires_at,
        }
    }
    pub fn verify(value: &str, secret: &str, now: i64) -> Option<Self> {
        if value.len() > 160 {
            return None;
        }
        let (payload, signature) = value.rsplit_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let mut signer = mac(secret);
        signer.update(payload.as_bytes());
        signer.verify_slice(&signature).ok()?;
        let mut parts = payload.split('.');
        if parts.next() != Some("v1") {
            return None;
        }
        let expires_at = parts.next()?.parse::<i64>().ok()?;
        if expires_at <= now || expires_at > now.checked_add(LIFETIME_SECONDS)? {
            return None;
        }
        let nonce = parts.next()?;
        if parts.next().is_some() || URL_SAFE_NO_PAD.decode(nonce).ok()?.len() != 32 {
            return None;
        }
        Some(Self {
            value: value.into(),
            nonce: nonce.into(),
            expires_at,
        })
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn cookie(&self, https: bool, now: i64) -> String {
        format!(
            "{COOKIE_NAME}={}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}{}",
            self.value,
            (self.expires_at - now).max(0),
            if https { "; Secure" } else { "" }
        )
    }
}

/// Serialize known-browser admission with the active account and enqueue the
/// event in that same commit. No rollout seed or witness-less suppression.
/// None means retirement/revocation won before this completion could publish.
pub async fn complete(
    db: &DatabaseConnection,
    user_id: i64,
    session_version: i64,
    witness: &Witness,
    method: &str,
) -> Result<Option<bool>> {
    rg_db::contention::retry_transaction("record completed browser login", || async {
        let txn = db.begin().await?;
        let Some(user) = queue::lock_account(&txn, user_id).await? else {
            return Ok(None);
        };
        if user.session_version != session_version {
            return Ok(None);
        }
        let now = chrono::Utc::now();
        let digest = rg_db::token_digest(&format!("browser:{user_id}:{}", witness.nonce));
        let known = known_browser::Entity::find()
            .filter(known_browser::Column::UserId.eq(user_id))
            .filter(known_browser::Column::WitnessHash.eq(&digest))
            .one(&txn)
            .await?;
        if known.as_ref().is_some_and(|row| row.expires_at > now) {
            txn.commit().await?;
            return Ok(Some(false));
        }
        if let Some(known) = known {
            known_browser::Entity::delete_by_id(known.id)
                .exec(&txn)
                .await?;
        }
        let Some(expires_at) =
            chrono::DateTime::from_timestamp(witness.expires_at, 0).filter(|at| *at > now)
        else {
            return Err(crate::error::invalid_request("browser witness expired"));
        };
        known_browser::Entity::insert(known_browser::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user_id),
            witness_hash: Set(digest),
            created_at: Set(now),
            expires_at: Set(expires_at),
        })
        .exec(&txn)
        .await?;
        let mut event = queue::QueueEvent::security(user_id, Event::NewDevice, None);
        event.metadata.method = Some(method.into());
        queue::enqueue(&txn, &event).await?;
        txn.commit().await?;
        Ok(Some(true))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseBackend, PaginatorTrait, Statement};
    #[test]
    fn witness_is_signed_domain_separated_absolute_and_cookie_is_private() {
        let now = chrono::Utc::now().timestamp();
        let witness = Witness::issue("fixture-key", now);
        assert!(Witness::verify(witness.value(), "fixture-key", now).is_some());
        assert!(Witness::verify(witness.value(), "other-key", now).is_none());
        assert!(Witness::verify(witness.value(), "fixture-key", now + LIFETIME_SECONDS).is_none());
        assert!(Witness::verify(&format!("{}x", witness.value()), "fixture-key", now).is_none());
        let parsed = Witness::verify(witness.value(), "fixture-key", now + 600).unwrap();
        assert_eq!(parsed.expires_at, witness.expires_at);
        let cookie = parsed.cookie(true, now + 600);
        for part in ["HttpOnly", "SameSite=Lax", "Path=/", "Secure"] {
            assert!(cookie.contains(part));
        }
        assert!(cookie.contains(&format!("Max-Age={}", LIFETIME_SECONDS - 600)));
        assert!(!parsed.cookie(false, now).contains("Secure"));
        let (payload, signature) = witness.value().rsplit_once('.').unwrap();
        let mut raw = Hmac::<Sha256>::new_from_slice(b"fixture-key").unwrap();
        raw.update(payload.as_bytes());
        assert!(raw
            .verify_slice(&URL_SAFE_NO_PAD.decode(signature).unwrap())
            .is_err());
    }
    #[tokio::test]
    async fn admission_is_atomic_account_scoped_and_every_fresh_witness_alerts() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let a =
            rg_db::ops::user_ops::create_user(&db, "browser-a", "a@example.invalid", "fixture", "")
                .await
                .unwrap();
        let b =
            rg_db::ops::user_ops::create_user(&db, "browser-b", "b@example.invalid", "fixture", "")
                .await
                .unwrap();
        let witness = Witness::issue("fixture", chrono::Utc::now().timestamp());
        assert_eq!(
            known_browser::Entity::find().count(&db).await.unwrap(),
            0,
            "migration/user creation does not seed known browsers"
        );
        let (one, two) = tokio::join!(
            complete(&db, a.id, a.session_version, &witness, "password"),
            complete(&db, a.id, a.session_version, &witness, "password")
        );
        assert_eq!(
            [one.unwrap(), two.unwrap()]
                .into_iter()
                .filter(|v| *v == Some(true))
                .count(),
            1
        );
        assert_eq!(
            rg_db::notification::scope(
                None,
                rg_db::notification::Metadata {
                    ip: Some("198.51.100.24".into()),
                    device: Some("New browser".into()),
                    ..Default::default()
                },
                complete(&db, b.id, b.session_version, &witness, "ldap")
            )
            .await
            .unwrap(),
            Some(true)
        );
        for _ in 0..2 {
            let fresh = Witness::issue("fixture", chrono::Utc::now().timestamp());
            assert_eq!(
                complete(&db, a.id, a.session_version, &fresh, "password")
                    .await
                    .unwrap(),
                Some(true)
            );
        }
        let rows = rg_db::entities::notification_delivery::Entity::find()
            .filter(rg_db::entities::notification_delivery::Column::EventKey.eq("new_device"))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(rows.len(), 4);
        let authenticated: rg_db::notification::Metadata = serde_json::from_str(
            &rows
                .iter()
                .find(|row| row.user_id == b.id)
                .unwrap()
                .metadata,
        )
        .unwrap();
        assert_eq!(authenticated.ip.as_deref(), Some("198.51.100.24"));
        assert_eq!(authenticated.device.as_deref(), Some("New browser"));
        assert!(rows
            .iter()
            .all(|row| !row.metadata.contains(witness.value())));
        rg_db::ops::user_ops::invalidate_sessions(&db, a.id)
            .await
            .unwrap();
        let fresh = Witness::issue("fixture", chrono::Utc::now().timestamp());
        assert_eq!(
            complete(&db, a.id, a.session_version, &fresh, "password")
                .await
                .unwrap(),
            None
        );
        assert_eq!(known_browser::Entity::find().count(&db).await.unwrap(), 4);
    }
    #[tokio::test]
    async fn failed_outbox_does_not_mark_browser_known() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "browser",
            "browser@example.invalid",
            "fixture",
            "",
        )
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "DROP TABLE notification_deliveries",
        ))
        .await
        .unwrap();
        let witness = Witness::issue("fixture", chrono::Utc::now().timestamp());
        assert!(
            complete(&db, user.id, user.session_version, &witness, "password")
                .await
                .is_err()
        );
        assert_eq!(known_browser::Entity::find().count(&db).await.unwrap(), 0);
    }
}
