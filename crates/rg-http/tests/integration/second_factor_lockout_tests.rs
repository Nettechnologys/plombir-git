//! Password-only doors must not erase the shared second-factor failure budget.

use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::ops::user_ops;

const PASSWORD: &str = "Qz7$wRtm";

async fn user(db: &rg_db::DatabaseConnection, id: i64) -> rg_db::entities::user::Model {
    user_ops::find_by_id(db, id).await.unwrap().unwrap()
}

#[tokio::test]
async fn password_only_doors_preserve_mfa_failures_until_the_account_locks() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    for door in ["registry", "initial"] {
        let login = format!("mfa-{door}");
        let (_, id) = register_full(&base, &login, &format!("{login}@example.test")).await;
        user_ops::enable_mfa(&db, id).await.unwrap();
        let login_response = client
            .post(format!("{base}/api/v1/users/login"))
            .json(&serde_json::json!({ "login": login, "password": PASSWORD }))
            .send()
            .await
            .unwrap();
        assert_eq!(login_response.status(), 200);
        let challenge = login_response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .find_map(|value| {
                let value = value.to_str().ok()?;
                value
                    .starts_with("plombir_git_mfa_challenge=")
                    .then(|| value.split(';').next().unwrap().to_owned())
            })
            .unwrap();
        for attempt in 1..=5 {
            let failed = client
                .post(format!("{base}/api/v1/users/mfa/verify"))
                .header(reqwest::header::COOKIE, &challenge)
                .json(
                    &serde_json::json!({ "username": login, "code": "not-a-code", "backup": true }),
                )
                .send()
                .await
                .unwrap();
            assert_eq!(failed.status(), 401);
            let account = user(&db, id).await;
            assert_eq!(account.login_attempts, attempt, "{door}");
            assert_eq!(account.locked_until.is_some(), attempt == 5, "{door}");

            let response =
                if door == "registry" {
                    client
                        .get(format!("{base}/v2/auth/token"))
                        .query(&[("service", "plombir-git-registry")])
                        .basic_auth(&login, Some(PASSWORD))
                        .send()
                        .await
                        .unwrap()
                } else {
                    client.post(format!("{base}/api/v1/users/password/initial"))
                    .json(&serde_json::json!({
                        "login": login, "password": PASSWORD, "new_password": "Different9$Password"
                    })).send().await.unwrap()
                };
            assert_eq!(
                response.status(),
                if door == "registry" || attempt == 5 {
                    401
                } else {
                    409
                }
            );
            let after_password = user(&db, id).await;
            assert_eq!(
                after_password.login_attempts, attempt,
                "{door} erased MFA failures"
            );
            assert_eq!(
                after_password.locked_until, account.locked_until,
                "{door} erased the lock"
            );
        }
    }
}

#[tokio::test]
async fn sudo_password_does_not_reset_repeated_second_factor_failures() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, id) = register_full(&base, "sudo-strikes", "sudo-strikes@example.test").await;
    user_ops::enable_mfa(&db, id).await.unwrap();
    let client = reqwest::Client::new();
    for attempt in 1..=5 {
        let response = client
            .post(format!("{base}/api/v1/users/me/sudo"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "password": PASSWORD, "backup_code": "not-a-code" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let account = user(&db, id).await;
        assert_eq!(account.login_attempts, attempt);
        assert_eq!(account.locked_until.is_some(), attempt == 5);
    }
}

#[tokio::test]
async fn reauthentication_cannot_reset_failures_for_a_changed_account() {
    use rg_db::{
        entities::user as account,
        sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter},
    };
    let (base, db) = spawn_test_app_with_db().await;
    for change in [
        "version", "factor", "password", "disabled", "mfa-off", "retired", "locked",
    ] {
        let login = format!("proof-{change}");
        let (_, id) = register_full(&base, &login, &format!("{login}@example.test")).await;
        user_ops::enable_mfa(&db, id).await.unwrap();
        user_ops::record_failed_login(&db, id, 5).await.unwrap();
        let proved = user(&db, id).await;
        let update = account::Entity::update_many();
        let update = match change {
            "version" => update.col_expr(
                account::Column::SessionVersion,
                Expr::value(proved.session_version + 1),
            ),
            "factor" => update.col_expr(
                account::Column::TotpSecret,
                Expr::value("replacement-factor"),
            ),
            "password" => update.col_expr(
                account::Column::PasswordHash,
                Expr::value("replacement-hash"),
            ),
            "disabled" => update.col_expr(account::Column::IsActive, Expr::value(false)),
            "mfa-off" => update.col_expr(account::Column::MfaEnabled, Expr::value(false)),
            "retired" => update.col_expr(
                account::Column::DeletedAt,
                Expr::value(Some(chrono::Utc::now())),
            ),
            "locked" => update.col_expr(
                account::Column::LockedUntil,
                Expr::value(Some(chrono::Utc::now() + chrono::Duration::minutes(15))),
            ),
            _ => unreachable!(),
        };
        update
            .filter(account::Column::Id.eq(id))
            .exec(&db)
            .await
            .unwrap();
        assert!(
            user_ops::reset_login_failures_after_reauthentication(&db, &proved)
                .await
                .unwrap()
                .is_none(),
            "{change}"
        );
        assert_eq!(user(&db, id).await.login_attempts, 1, "{change}");
        if change == "locked" {
            assert!(user(&db, id).await.locked_until.unwrap() > chrono::Utc::now());
        }
    }
}
