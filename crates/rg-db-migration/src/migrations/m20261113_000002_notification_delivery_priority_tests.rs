use rg_db::notification::{Event, PRIORITY_ACCOUNT, PRIORITY_SHARED};
use rg_db::ops::notification_delivery_ops as queue;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, EntityTrait};

use super::{Migration, ACCOUNT_EVENTS, SHARED_EVENTS};

fn keys(class: i16) -> Vec<&'static str> {
    let mut keys: Vec<_> = Event::ALL
        .iter()
        .filter(|event| event.priority() == class)
        .map(|event| event.key())
        .collect();
    keys.sort_unstable();
    keys
}

fn sorted(list: &[&'static str]) -> Vec<&'static str> {
    let mut list = list.to_vec();
    list.sort_unstable();
    list
}

/// The migration classifies rows already queued from a frozen copy of
/// `Event::priority`. The copy must equal the code it was frozen from; when
/// the code moves on, a new migration reclassifies, this one stays.
#[test]
fn the_frozen_classes_match_the_code() {
    assert_eq!(sorted(ACCOUNT_EVENTS), keys(PRIORITY_ACCOUNT));
    assert_eq!(sorted(SHARED_EVENTS), keys(PRIORITY_SHARED));
    for event in Event::ALL {
        assert_eq!(
            event.priority() == rg_db::notification::PRIORITY_ACTIVITY,
            !event.security(),
            "{event:?}: every security event is in class 0 or 1, every other in 2"
        );
    }
}

/// Queued and in-flight rows get their class; terminal rows are left alone,
/// and running `up` again changes nothing.
#[tokio::test]
async fn queued_rows_are_classified_and_terminal_rows_are_not() {
    let db = rg_db::connect_with_pool("sqlite::memory:", rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();
    crate::run_migrations(&db).await.unwrap();
    let user = rg_db::ops::user_ops::create_user(&db, "classes", "c@example.invalid", "x", "C")
        .await
        .unwrap();
    let mut ids = Vec::new();
    for event in [
        Event::NewDevice,
        Event::OrganizationAccessChanged,
        Event::Participating,
        Event::PasswordChanged,
    ] {
        let queued = queue::QueueEvent::security(user.id, event, None);
        queue::enqueue(&db, &queued).await.unwrap();
        ids.push(queue::event_rows(&db, user.id, &queued.id).await.unwrap()[0].id);
    }
    // The state before this migration: no class anywhere, one row in flight
    // and one already delivered.
    db.execute_unprepared("UPDATE notification_deliveries SET priority = 2")
        .await
        .unwrap();
    db.execute_unprepared(&format!(
        "UPDATE notification_deliveries SET status = 'inflight' WHERE id = {}",
        ids[1]
    ))
    .await
    .unwrap();
    db.execute_unprepared(&format!(
        "UPDATE notification_deliveries SET status = 'sent' WHERE id = {}",
        ids[3]
    ))
    .await
    .unwrap();

    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    let class = |id: i64| {
        let db = &db;
        async move {
            rg_db::entities::notification_delivery::Entity::find_by_id(id)
                .one(db)
                .await
                .unwrap()
                .unwrap()
                .priority
        }
    };
    assert_eq!(class(ids[0]).await, 0, "queued new_device");
    assert_eq!(
        class(ids[1]).await,
        1,
        "in-flight organization_access_changed"
    );
    assert_eq!(class(ids[2]).await, 2, "queued participating");
    assert_eq!(class(ids[3]).await, 2, "a delivered row is left alone");
    assert!(manager
        .has_index("notification_deliveries", "idx_notification_delivery_class")
        .await
        .unwrap());

    Migration.down(&manager).await.unwrap();
    assert!(!manager
        .has_column("notification_deliveries", "priority")
        .await
        .unwrap());
}
