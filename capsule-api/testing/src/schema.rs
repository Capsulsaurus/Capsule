#[cfg(test)]
mod tests {
    use crate::common;
    use entity::user::{ActiveModel as UserActiveModel, Column as UserColumn, Entity as User};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use uuid::Uuid;

    #[tokio::test]
    async fn test_create_user() {
        let db = common::setup_test_db().await.expect("setup db");

        let test_email = format!("test.{}@example.com", Uuid::new_v4());

        let user = UserActiveModel {
            username: Set(format!("testuser_{}", Uuid::new_v4())),
            name: Set("Test User".to_string()),
            email: Set(test_email.clone()),
            account_verified: Set(false),
            needs_onboarding: Set(true),
            password_hash: Set("hash123".to_string()),
            is_admin: Set(false),
            ..Default::default()
        };

        let inserted = user.insert(&db).await.expect("insert");
        assert!(!inserted.id.is_empty());

        let found = User::find()
            .filter(UserColumn::Email.eq(test_email.clone()))
            .one(&db)
            .await
            .expect("query")
            .expect("not found");

        assert_eq!(found.email, test_email);
    }

    #[tokio::test]
    async fn test_email_uniqueness() {
        let db = common::setup_test_db().await.expect("setup db");

        let test_email = format!("unique.{}@example.com", Uuid::new_v4());

        let user1 = UserActiveModel {
            username: Set(format!("user1_{}", Uuid::new_v4())),
            name: Set("User One".to_string()),
            email: Set(test_email.clone()),
            account_verified: Set(false),
            needs_onboarding: Set(true),
            password_hash: Set("hash123".to_string()),
            is_admin: Set(false),
            ..Default::default()
        };

        user1.insert(&db).await.expect("insert first");

        let user2 = UserActiveModel {
            username: Set(format!("user2_{}", Uuid::new_v4())),
            name: Set("User Two".to_string()),
            email: Set(test_email.clone()),
            account_verified: Set(false),
            needs_onboarding: Set(true),
            password_hash: Set("hash456".to_string()),
            is_admin: Set(false),
            ..Default::default()
        };

        let res = user2.insert(&db).await;
        assert!(res.is_err(), "expected uniqueness violation");
    }

    #[tokio::test]
    async fn test_email_uniqueness_case_insensitive() {
        let db = common::setup_test_db().await.expect("setup db");

        let base = format!("case.{}@example.com", Uuid::new_v4());
        let lower = base.to_lowercase();
        let upper = base.to_uppercase();

        let user1 = UserActiveModel {
            username: Set(format!("lc_{}", Uuid::new_v4())),
            name: Set("Lower Case".to_string()),
            email: Set(lower.clone()),
            account_verified: Set(false),
            needs_onboarding: Set(true),
            password_hash: Set("hash123".to_string()),
            is_admin: Set(false),
            ..Default::default()
        };

        user1.insert(&db).await.expect("insert first");

        let user2 = UserActiveModel {
            username: Set(format!("uc_{}", Uuid::new_v4())),
            name: Set("Upper Case".to_string()),
            email: Set(upper.clone()),
            account_verified: Set(false),
            needs_onboarding: Set(true),
            password_hash: Set("hash456".to_string()),
            is_admin: Set(false),
            ..Default::default()
        };

        let res = user2.insert(&db).await;
        assert!(
            res.is_err(),
            "expected case-insensitive uniqueness violation"
        );
    }
}
