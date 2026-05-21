use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

// TODO: Check
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(64))", indexed)]
    pub username: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(255))")]
    pub name: String,
    /// Note: uniqueness is enforced by a case-insensitive database index on LOWER(email) in migration
    #[sea_orm(column_type = "String(StringLen::N(255))")]
    pub email: String, // TODO: Write unit test on unique constraint
    /// URL to profile image
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub profile_image_url: Option<String>,
    /// Whether the user's email has been verified
    pub account_verified: bool,
    /// Whether the user needs to go through onboarding
    pub needs_onboarding: bool,
    #[sea_orm(unique, column_type = "String(StringLen::N(255))")]
    pub password_hash: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(255))", nullable)]
    pub totp_secret: Option<String>,
    #[sea_orm(unique, column_type = "String(StringLen::N(255))", nullable)]
    pub totp_verified: Option<bool>,
    #[sea_orm(unique, column_type = "String(StringLen::N(255))", nullable)]
    pub password_reset_token: Option<String>,
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub password_reset_expires_at: Option<DateTime<Utc>>,
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub last_login_at: Option<DateTime<Utc>>,
    #[sea_orm(default_value = "0")]
    pub failed_login_attempts: i32,
    // pub external_id: String,
    #[sea_orm(indexed)]
    pub is_admin: bool,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP",
        on_update = "CURRENT_TIMESTAMP"
    )]
    pub modified_at: DateTime<Utc>,
    /// Date when the user was deleted if not NULL
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable, indexed)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// How the account was created (e.g. 'invitation'); NULL for seeded/legacy/unknown
    #[sea_orm(column_type = "String(StringLen::N(32))", nullable)]
    pub registered_via: Option<String>,
}

impl Model {
    pub fn profile_image_url(&self) -> Option<String> {
        self.profile_image_url.clone() // TODO: Need to process this properly to ensure access for public
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::owner_member::Entity")]
    OwnerMembers,
}

impl Related<super::owner_member::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::OwnerMembers.def()
    }
}

impl ActiveModelBehavior for ActiveModel {
    fn new() -> Self {
        Self {
            id: Set(nanoid!()),
            ..ActiveModelTrait::default()
        }
    }
}

impl Entity {
    pub fn find_by_username(username: &str) -> Select<Entity> {
        Self::find().filter(Column::Username.eq(username))
    }

    pub fn find_by_email(email: &str) -> Select<Entity> {
        Self::find().filter(Column::Email.eq(email))
    }

    /// Case-insensitive search for email using LOWER(email)
    pub fn find_by_email_ci(email: &str) -> Select<Entity> {
        use sea_orm::sea_query::Expr;
        let lower_email = email.to_lowercase();
        Self::find().filter(Expr::cust("LOWER(email)").eq(lower_email))
    }

    // TODO: Write tests on these ^^
}
