use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::face::ActiveModel as FaceActiveModel;
use entity::face::Model as FaceModel;
use entity::person::ActiveModel as PersonActiveModel;
use entity::person::Model as PersonModel;
use entity::{asset, face, person};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter,
    QuerySelect, RelationTrait, Set,
};
use serde::{Deserialize, Serialize};

use super::asset::{AssetMetadata, BoundingBox};
use crate::context::AppContext;

/// A recognized person in the photo library
pub struct Person {
    pub model: PersonModel,
}

#[Object]
impl Person {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    async fn name(&self) -> Option<&String> {
        self.model.name.as_ref()
    }

    // TODO: Resolve cover photo via dataloader
    async fn cover_photo(&self, ctx: &Context<'_>) -> Result<Option<AssetMetadata>> {
        if let Some(cover_id) = &self.model.cover_photo_id {
            let app_ctx = ctx.data::<AppContext>()?;
            let asset = asset::Entity::find_by_id(cover_id)
                .one(&app_ctx.db.conn)
                .await?;
            Ok(asset.map(|model| AssetMetadata { model }))
        } else {
            Ok(None)
        }
    }

    async fn is_hidden(&self) -> bool {
        self.model.is_hidden
    }

    async fn face_count(&self) -> i32 {
        self.model.face_count
    }

    async fn created_at(&self) -> DateTime<Utc> {
        self.model.created_at
    }

    /// Assets containing this person
    async fn assets(&self, ctx: &Context<'_>) -> Result<Vec<AssetMetadata>> {
        let app_ctx = ctx.data::<AppContext>()?;
        // Find faces for this person
        let faces = face::Entity::find()
            .filter(face::Column::PersonId.eq(&self.model.id))
            .all(&app_ctx.db.conn)
            .await?;

        // Collect asset IDs
        let asset_ids: Vec<String> = faces.into_iter().map(|f| f.asset_id).collect();
        if asset_ids.is_empty() {
            return Ok(vec![]);
        }

        // Fetch assets (deduplicated by nature of ID list if distinct, but sea_orm `is_in` doesn't dedup result if input is dup,
        // however we query the Asset table, so result is unique per ID)
        // Actually asset_ids might have duplicates if multiple faces of same person in one asset (rare but possible).
        // Let's dedup ids.
        use std::collections::HashSet;
        let unique_ids: Vec<String> = asset_ids
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let assets = asset::Entity::find()
            .filter(asset::Column::Id.is_in(unique_ids))
            .all(&app_ctx.db.conn)
            .await?;

        Ok(assets
            .into_iter()
            .map(|model| AssetMetadata { model })
            .collect())
    }
}

/// A detected face within an asset
#[derive(SimpleObject)]
#[graphql(complex)]
pub struct Face {
    pub id: ID,
    pub confidence: f64,
    pub is_confirmed: bool,
    #[graphql(skip)]
    pub model: FaceModel,
}

#[ComplexObject]
impl Face {
    async fn bounding_box(&self) -> Result<BoundingBox> {
        // Parse JSON bounding box
        serde_json::from_str(&self.model.bounding_box)
            .map_err(|e| Error::new(format!("Failed to parse bounding box: {}", e)))
    }

    async fn asset(&self, ctx: &Context<'_>) -> Result<AssetMetadata> {
        let app_ctx = ctx.data::<AppContext>()?;
        let asset = asset::Entity::find_by_id(&self.model.asset_id)
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Asset not found"))?;
        Ok(AssetMetadata { model: asset })
    }

    async fn person(&self, ctx: &Context<'_>) -> Result<Option<Person>> {
        if let Some(person_id) = &self.model.person_id {
            let app_ctx = ctx.data::<AppContext>()?;
            let person = person::Entity::find_by_id(person_id)
                .one(&app_ctx.db.conn)
                .await?;
            Ok(person.map(|model| Person { model }))
        } else {
            Ok(None)
        }
    }
}

// ===== Inputs =====

#[derive(InputObject)]
pub struct UpdatePersonInput {
    pub name: Option<String>,
    pub is_hidden: Option<bool>,
    pub cover_photo_id: Option<ID>,
}

#[derive(InputObject, Serialize, Deserialize)]
pub struct FaceBoundingBoxInput {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// ===== Query =====

#[derive(Default)]
pub struct PersonQuery;

#[Object]
impl PersonQuery {
    /// Get person by ID
    async fn person(&self, ctx: &Context<'_>, id: ID) -> Result<Person> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        // Check ownership? Theoretically yes, but Person table has owner_id.
        let person = person::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Person not found"))?;

        if person.owner_id != *user_id {
            return Err(Error::new("Permission denied"));
        }

        Ok(Person { model: person })
    }

    /// Get all people (optionally including hidden)
    async fn people(&self, ctx: &Context<'_>, include_hidden: Option<bool>) -> Result<Vec<Person>> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let mut query = person::Entity::find().filter(person::Column::OwnerId.eq(user_id));

        if !include_hidden.unwrap_or(false) {
            query = query.filter(person::Column::IsHidden.eq(false));
        }

        let people = query.all(&app_ctx.db.conn).await?;
        Ok(people.into_iter().map(|model| Person { model }).collect())
    }

    /// Search people by name
    async fn search_people(&self, ctx: &Context<'_>, query: String) -> Result<Vec<Person>> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let people = person::Entity::find()
            .filter(person::Column::OwnerId.eq(user_id))
            .filter(person::Column::Name.contains(&query)) // Case sensitive or insensitive? SeaORM 'contains' translates to LIKE '%q%'
            .all(&app_ctx.db.conn)
            .await?;

        // TODO: Case insensitive search would be better: Expr::cust("LOWER(name) LIKE ...")

        Ok(people.into_iter().map(|model| Person { model }).collect())
    }

    /// Get unassigned faces (faces not yet linked to a person)
    async fn unassigned_faces(&self, ctx: &Context<'_>, limit: Option<i32>) -> Result<Vec<Face>> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        // Join with Asset to ensure ownership
        let faces = face::Entity::find()
            .join(sea_orm::JoinType::InnerJoin, face::Relation::Asset.def())
            .filter(asset::Column::OwnerId.eq(user_id))
            .filter(face::Column::PersonId.is_null())
            .limit(limit.map(|l| l as u64))
            .all(&app_ctx.db.conn)
            .await?;

        Ok(faces
            .into_iter()
            .map(|model| Face {
                id: ID::from(&model.id),
                confidence: model.confidence,
                is_confirmed: model.is_confirmed,
                model,
            })
            .collect())
    }
}

// ===== Mutations =====

#[derive(Default)]
pub struct PersonMutation;

#[Object]
impl PersonMutation {
    /// Merge multiple people into one (e.g., "Mom" and "Mother")
    async fn merge_people(
        &self,
        ctx: &Context<'_>,
        source_ids: Vec<ID>,
        target_id: ID,
    ) -> Result<Person> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;
        let db = &app_ctx.db.conn;

        // Verify target person ownership
        let target = person::Entity::find_by_id(target_id.to_string())
            .filter(person::Column::OwnerId.eq(user_id))
            .one(db)
            .await?
            .ok_or_else(|| Error::new("Target person not found"))?;

        // Reassign faces from source persons to target
        let source_ids_str: Vec<String> = source_ids.iter().map(|id| id.to_string()).collect();

        // Check ownership of all source persons?
        let count = person::Entity::find()
            .filter(person::Column::Id.is_in(source_ids_str.clone()))
            .filter(person::Column::OwnerId.eq(user_id))
            .count(db)
            .await?;

        if count != source_ids.len() as u64 {
            return Err(Error::new(
                "Some source persons not found or permission denied",
            ));
        }

        // Update faces
        face::Entity::update_many()
            .col_expr(
                face::Column::PersonId,
                sea_orm::sea_query::Expr::value(target.id.clone()),
            )
            .filter(face::Column::PersonId.is_in(source_ids_str.clone()))
            .exec(db)
            .await?;

        // Delete source persons
        person::Entity::delete_many()
            .filter(person::Column::Id.is_in(source_ids_str))
            .exec(db)
            .await?;

        // Re-fetch target to update face count (if we had a trigger or we calculate it manually)
        // For now returning the original object, frontend might need to refetch.
        // TODO: Update face_count for target person

        Ok(Person { model: target })
    }

    /// Update person name or visibility
    async fn update_person(
        &self,
        ctx: &Context<'_>,
        id: ID,
        input: UpdatePersonInput,
    ) -> Result<Person> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let person = person::Entity::find_by_id(id.to_string())
            .filter(person::Column::OwnerId.eq(user_id))
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Person not found"))?;

        let mut active: PersonActiveModel = person.into_active_model();

        if let Some(name) = input.name {
            active.name = Set(Some(name));
        }
        if let Some(hidden) = input.is_hidden {
            active.is_hidden = Set(hidden);
        }
        if let Some(cover) = input.cover_photo_id {
            active.cover_photo_id = Set(Some(cover.to_string()));
        }

        let updated = active.update(&app_ctx.db.conn).await?;
        Ok(Person { model: updated })
    }

    /// Delete a person
    async fn delete_person(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let person = person::Entity::find_by_id(id.to_string())
            .filter(person::Column::OwnerId.eq(user_id))
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Person not found"))?;

        // Unassign faces
        face::Entity::update_many()
            .col_expr(
                face::Column::PersonId,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .filter(face::Column::PersonId.eq(&person.id))
            .exec(&app_ctx.db.conn)
            .await?;

        // Delete person
        let res = person::Entity::delete_by_id(person.id)
            .exec(&app_ctx.db.conn)
            .await?;

        Ok(res.rows_affected > 0)
    }

    /// Assign a face to a person
    async fn assign_face(&self, ctx: &Context<'_>, face_id: ID, person_id: ID) -> Result<Face> {
        let app_ctx = ctx.data::<AppContext>()?;

        let face = face::Entity::find_by_id(face_id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Face not found"))?;

        // TODO: Verify ownership of face (via asset)?

        let mut active: FaceActiveModel = face.into_active_model();
        active.person_id = Set(Some(person_id.to_string()));
        active.is_confirmed = Set(true);

        let updated = active.update(&app_ctx.db.conn).await?;

        Ok(Face {
            id: ID::from(&updated.id),
            confidence: updated.confidence,
            is_confirmed: updated.is_confirmed,
            model: updated,
        })
    }

    /// Create a face manually (tagging)
    async fn create_face(
        &self,
        ctx: &Context<'_>,
        asset_id: ID,
        bounding_box: FaceBoundingBoxInput,
    ) -> Result<Face> {
        let app_ctx = ctx.data::<AppContext>()?;

        // Verify asset exists and owned by user
        let asset = asset::Entity::find_by_id(asset_id.to_string())
            .filter(asset::Column::OwnerId.eq(app_ctx.user.user_id()?))
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Asset not found"))?;

        let bbox_json = serde_json::to_string(&bounding_box)
            .map_err(|e| Error::new(format!("Failed to serialize bounding box: {}", e)))?;

        let model = FaceActiveModel {
            asset_id: Set(asset.id),
            bounding_box: Set(bbox_json),
            confidence: Set(1.0), // Manual tag = 100% confidence
            is_confirmed: Set(true),
            person_id: Set(None), // Initially unassigned, or maybe we should pass person_id?
            ..Default::default()
        };

        let saved = model.insert(&app_ctx.db.conn).await?;

        Ok(Face {
            id: ID::from(&saved.id),
            confidence: saved.confidence,
            is_confirmed: saved.is_confirmed,
            model: saved,
        })
    }

    /// Confirm or reject a suggested face assignment
    async fn confirm_face(&self, ctx: &Context<'_>, face_id: ID, confirmed: bool) -> Result<Face> {
        let app_ctx = ctx.data::<AppContext>()?;

        let face = face::Entity::find_by_id(face_id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Face not found"))?;

        let mut active: FaceActiveModel = face.into_active_model();
        if confirmed {
            active.is_confirmed = Set(true);
        } else {
            // Rejecting usually means unassigning?
            active.person_id = Set(None);
            active.is_confirmed = Set(false);
        }

        let updated = active.update(&app_ctx.db.conn).await?;

        Ok(Face {
            id: ID::from(&updated.id),
            confidence: updated.confidence,
            is_confirmed: updated.is_confirmed,
            model: updated,
        })
    }

    /// Remove a face from a person (marks as unassigned)
    async fn unassign_face(&self, ctx: &Context<'_>, face_id: ID) -> Result<Face> {
        let app_ctx = ctx.data::<AppContext>()?;

        let face = face::Entity::find_by_id(face_id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Face not found"))?;

        let mut active: FaceActiveModel = face.into_active_model();
        active.person_id = Set(None);
        active.is_confirmed = Set(false);

        let updated = active.update(&app_ctx.db.conn).await?;

        Ok(Face {
            id: ID::from(&updated.id),
            confidence: updated.confidence,
            is_confirmed: updated.is_confirmed,
            model: updated,
        })
    }

    /// Delete a face tag
    async fn delete_face(&self, ctx: &Context<'_>, face_id: ID) -> Result<bool> {
        let app_ctx = ctx.data::<AppContext>()?;

        // TODO: Ownership check
        let res = face::Entity::delete_by_id(face_id.to_string())
            .exec(&app_ctx.db.conn)
            .await?;

        Ok(res.rows_affected > 0)
    }
}
