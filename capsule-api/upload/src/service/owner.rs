use crate::error::UploadError;
use chrono::Utc;
use entity::{owner, owner_member};
use nanoid::nanoid;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, Set,
};

#[allow(dead_code)]
#[derive(Clone)]
pub struct OwnerService {
    _conn: DatabaseConnection,
}

#[allow(dead_code)]
impl OwnerService {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self { _conn: conn }
    }

    /// Gets an existing owner for a set of users or creates a new one.
    /// This finds an owner group that contains EXACTLY the specified users.
    // TODO: Optimize this function
    pub async fn get_or_create_owner(
        &self,
        user_ids: &[String],
        txn: &DatabaseTransaction,
    ) -> Result<String, UploadError> {
        if user_ids.is_empty() {
            return Err(UploadError::InvalidUpload(
                "Cannot create owner for empty user list".to_string(),
            ));
        }

        // 1. Find potential owner groups (those containing the first user)
        // Optimization: We only search groups involving the first user.
        // A group strict-matching [A, B] MUST include A.
        use sea_orm::QuerySelect;
        use std::collections::HashSet;

        let first_user = &user_ids[0];
        let candidates: Vec<String> = owner_member::Entity::find()
            .select_only()
            .column(owner_member::Column::OwnerId)
            .filter(owner_member::Column::UserId.eq(first_user))
            .into_tuple()
            .all(txn)
            .await?;

        let target_users: HashSet<&String> = user_ids.iter().collect();

        // 2. Check strict equality for each candidate
        for owner_id in candidates {
            let members: Vec<String> = owner_member::Entity::find()
                .select_only()
                .column(owner_member::Column::UserId)
                .filter(owner_member::Column::OwnerId.eq(&owner_id))
                .into_tuple()
                .all(txn)
                .await?;

            if members.len() == target_users.len() {
                let current_users: HashSet<&String> = members.iter().collect();
                if current_users == target_users {
                    return Ok(owner_id);
                }
            }
        }

        // 3. If not found, create a new Owner
        let owner_id = nanoid!();
        let owner = owner::ActiveModel {
            id: Set(owner_id.clone()),
            created_at: Set(Utc::now()),
        };
        owner.insert(txn).await?;

        // 4. Create OwnerMember for each user
        for uid in user_ids {
            let member = owner_member::ActiveModel {
                owner_id: Set(owner_id.clone()),
                user_id: Set(uid.clone()),
                created_at: Set(Utc::now()),
                ..Default::default()
            };
            member.insert(txn).await?;
        }

        Ok(owner_id)
    }
}
