use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Install required extensions
        db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS pg_trgm;")
            .await?;
        db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS btree_gin;")
            .await?;

        // ========================================
        // 1. Create Owners Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Owners::Table)
                    .if_not_exists()
                    .col(char_len(Owners::Id, 21).primary_key())
                    .col(
                        timestamp_with_time_zone(Owners::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 2. Create Users Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(char_len(Users::Id, 21).primary_key())
                    .col(string_len(Users::Username, 64))
                    .col(string(Users::Name))
                    .col(string_len(Users::Email, 255))
                    .col(string_len_null(Users::ProfileImageUrl, 255))
                    .col(boolean(Users::AccountVerified).default(false))
                    .col(boolean(Users::NeedsOnboarding).default(true))
                    .col(string(Users::PasswordHash))
                    .col(string_len_null(Users::TotpSecret, 255))
                    .col(boolean(Users::TotpVerified).default(false))
                    .col(string_null(Users::PasswordResetToken).unique_key())
                    .col(timestamp_with_time_zone_null(Users::PasswordResetExpiresAt))
                    .col(timestamp_with_time_zone_null(Users::LastLoginAt))
                    .col(integer(Users::FailedLoginAttempts).default(0))
                    .col(boolean(Users::IsAdmin).default(false))
                    .col(
                        timestamp_with_time_zone(Users::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(Users::ModifiedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(timestamp_with_time_zone_null(Users::DeletedAt))
                    .to_owned(),
            )
            .await?;

        // Create users indices
        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username_lower ON users (LOWER(username))"#,
        )
        .await?;
        db.execute_unprepared(
            r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_lower ON users (LOWER(email))"#,
        )
        .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_users_deleted_at")
                    .table(Users::Table)
                    .col(Users::DeletedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_users_is_admin")
                    .table(Users::Table)
                    .col(Users::IsAdmin)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 3. Create Owner Members Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(OwnerMembers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OwnerMembers::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(char_len(OwnerMembers::OwnerId, 21))
                    .col(char_len(OwnerMembers::UserId, 21))
                    .col(
                        timestamp_with_time_zone(OwnerMembers::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_owner_members_owner_id")
                            .from(OwnerMembers::Table, OwnerMembers::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_owner_members_user_id")
                            .from(OwnerMembers::Table, OwnerMembers::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("idx_owner_members_owner_user_unique")
                            .table(OwnerMembers::Table)
                            .col(OwnerMembers::OwnerId)
                            .col(OwnerMembers::UserId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 3.5. Create Friendships Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Friendships::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Friendships::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(char_len(Friendships::UserId, 21))
                    .col(char_len(Friendships::FriendId, 21))
                    .col(string_len(Friendships::Status, 20).default("pending"))
                    .col(
                        timestamp_with_time_zone(Friendships::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(timestamp_with_time_zone_null(Friendships::AcceptedAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_friendships_user_id")
                            .from(Friendships::Table, Friendships::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_friendships_friend_id")
                            .from(Friendships::Table, Friendships::FriendId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("idx_friendships_user_friend_unique")
                            .table(Friendships::Table)
                            .col(Friendships::UserId)
                            .col(Friendships::FriendId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create friendships indices
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_friendships_user_id")
                    .table(Friendships::Table)
                    .col(Friendships::UserId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_friendships_friend_id")
                    .table(Friendships::Table)
                    .col(Friendships::FriendId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_friendships_status")
                    .table(Friendships::Table)
                    .col(Friendships::Status)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 4. Create Albums Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Albums::Table)
                    .if_not_exists()
                    .col(char_len(Albums::Id, 21).primary_key())
                    .col(char_len(Albums::OwnerId, 21))
                    .col(string(Albums::Name))
                    .col(text(Albums::Description))
                    .col(
                        timestamp_with_time_zone(Albums::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(Albums::ModifiedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(timestamp_with_time_zone_null(Albums::DeletedAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_albums_owner_id")
                            .from(Albums::Table, Albums::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create albums indices
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_owner_id")
                    .table(Albums::Table)
                    .col(Albums::OwnerId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_name")
                    .table(Albums::Table)
                    .col(Albums::Name)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_description")
                    .table(Albums::Table)
                    .col(Albums::Description)
                    .index_type(IndexType::FullText)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_created_at")
                    .table(Albums::Table)
                    .col(Albums::CreatedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_modified_at")
                    .table(Albums::Table)
                    .col(Albums::ModifiedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_albums_deleted_at")
                    .table(Albums::Table)
                    .col(Albums::DeletedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 5. Create Album Shares Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(AlbumShares::Table)
                    .if_not_exists()
                    .col(char_len(AlbumShares::Id, 21).primary_key())
                    .col(char_len(AlbumShares::AlbumId, 21))
                    .col(char_len(AlbumShares::UserId, 21))
                    .col(string_len(AlbumShares::Permission, 10))
                    .col(
                        timestamp_with_time_zone(AlbumShares::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_album_shares_album_id")
                            .from(AlbumShares::Table, AlbumShares::AlbumId)
                            .to(Albums::Table, Albums::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_album_shares_user_id")
                            .from(AlbumShares::Table, AlbumShares::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 6. Create Assets Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Assets::Table)
                    .if_not_exists()
                    .col(char_len(Assets::Id, 21).primary_key())
                    .col(char_len(Assets::OwnerId, 21))
                    .col(char_len_null(Assets::AlbumId, 21))
                    .col(unsigned(Assets::Width))
                    .col(unsigned(Assets::Height))
                    .col(string_len(Assets::AssetType, 2))
                    .col(string(Assets::OriginalFilename))
                    .col(big_integer(Assets::FileSize))
                    .col(big_integer(Assets::FileHash)) // i64 hash stored as bigint
                    .col(string(Assets::ContentType))
                    // Geo-location for spatial queries
                    .col(double_null(Assets::Latitude))
                    .col(double_null(Assets::Longitude))
                    // Visual placeholders
                    .col(string_len_null(Assets::LqipHash, 50))
                    .col(string_len_null(Assets::DominantColor, 7))
                    // User preferences
                    .col(boolean(Assets::IsFavorite).default(false))
                    // Timestamps
                    .col(timestamp_with_time_zone_null(Assets::CapturedAt))
                    .col(
                        timestamp_with_time_zone(Assets::UploadedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(Assets::ModifiedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(timestamp_with_time_zone_null(Assets::DeletedAt))
                    .col(boolean(Assets::Uploaded))
                    .col(char_len(Assets::UploadUserId, 21))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_assets_upload_user_id")
                            .from(Assets::Table, Assets::UploadUserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_assets_owner_id")
                            .from(Assets::Table, Assets::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_assets_album_id")
                            .from(Assets::Table, Assets::AlbumId)
                            .to(Albums::Table, Albums::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create assets indices
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_owner_id")
                    .table(Assets::Table)
                    .col(Assets::OwnerId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_album_id")
                    .table(Assets::Table)
                    .col(Assets::AlbumId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_captured_at")
                    .table(Assets::Table)
                    .col(Assets::CapturedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_uploaded_at")
                    .table(Assets::Table)
                    .col(Assets::UploadedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_modified_at")
                    .table(Assets::Table)
                    .col(Assets::ModifiedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_deleted_at")
                    .table(Assets::Table)
                    .col(Assets::DeletedAt)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_is_favorite")
                    .table(Assets::Table)
                    .col(Assets::IsFavorite)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 6.5. Create Asset Stacks Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(AssetStacks::Table)
                    .if_not_exists()
                    .col(char_len(AssetStacks::Id, 21).primary_key())
                    .col(char_len(AssetStacks::OwnerId, 21))
                    .col(string_len(AssetStacks::StackType, 20))
                    .col(char_len(AssetStacks::PrimaryAssetId, 21))
                    .col(char_len_null(AssetStacks::CoverAssetId, 21))
                    .col(ColumnDef::new(AssetStacks::Metadata).json_binary().null())
                    .col(boolean(AssetStacks::IsCollapsed).default(true))
                    .col(boolean(AssetStacks::IsAutoGenerated).default(true))
                    .col(
                        timestamp_with_time_zone(AssetStacks::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(AssetStacks::ModifiedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_stacks_owner_id")
                            .from(AssetStacks::Table, AssetStacks::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_stacks_primary_asset_id")
                            .from(AssetStacks::Table, AssetStacks::PrimaryAssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_stacks_cover_asset_id")
                            .from(AssetStacks::Table, AssetStacks::CoverAssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create asset_stacks indices
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_stacks_owner_id")
                    .table(AssetStacks::Table)
                    .col(AssetStacks::OwnerId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_stacks_primary_asset_id")
                    .table(AssetStacks::Table)
                    .col(AssetStacks::PrimaryAssetId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_stacks_stack_type")
                    .table(AssetStacks::Table)
                    .col(AssetStacks::StackType)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 6.6. Create Stack Members Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(StackMembers::Table)
                    .if_not_exists()
                    .col(char_len(StackMembers::Id, 21).primary_key())
                    .col(char_len(StackMembers::StackId, 21))
                    .col(char_len(StackMembers::AssetId, 21))
                    .col(integer(StackMembers::SequenceOrder))
                    .col(string_len(StackMembers::MemberRole, 16))
                    .col(ColumnDef::new(StackMembers::Metadata).json_binary().null())
                    .col(
                        timestamp_with_time_zone(StackMembers::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_stack_members_stack_id")
                            .from(StackMembers::Table, StackMembers::StackId)
                            .to(AssetStacks::Table, AssetStacks::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_stack_members_asset_id")
                            .from(StackMembers::Table, StackMembers::AssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("idx_stack_members_stack_asset_unique")
                            .table(StackMembers::Table)
                            .col(StackMembers::StackId)
                            .col(StackMembers::AssetId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create stack_members indices
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_stack_members_stack_id")
                    .table(StackMembers::Table)
                    .col(StackMembers::StackId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_stack_members_asset_id")
                    .table(StackMembers::Table)
                    .col(StackMembers::AssetId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 6.7. Add Stack Columns to Assets Table
        // ========================================
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .add_column(char_len_null(Assets::StackId, 21))
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk_assets_stack_id")
                            .from_tbl(Assets::Table)
                            .from_col(Assets::StackId)
                            .to_tbl(AssetStacks::Table)
                            .to_col(AssetStacks::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .add_column(boolean(Assets::IsStackHidden).default(false))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_stack_id")
                    .table(Assets::Table)
                    .col(Assets::StackId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 6.8. Composite Indices for Album+Stack Queries (Efficiency)
        // ========================================
        // Composite index for "get all stacks in album" queries
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_album_id_stack_id")
                    .table(Assets::Table)
                    .col(Assets::AlbumId)
                    .col(Assets::StackId)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;

        // Partial index for "get stacked assets for owner" queries
        // Only indexes rows where stack_id IS NOT NULL for efficiency
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_assets_owner_stacked 
            ON assets (owner_id, stack_id) 
            WHERE stack_id IS NOT NULL"#,
        )
        .await?;

        // ========================================
        // 7. Create People Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(People::Table)
                    .if_not_exists()
                    .col(char_len(People::Id, 21).primary_key())
                    .col(char_len(People::OwnerId, 21))
                    .col(string_len_null(People::Name, 255))
                    .col(char_len_null(People::CoverPhotoId, 21))
                    .col(boolean(People::IsHidden).default(false))
                    .col(integer(People::FaceCount).default(0))
                    .col(
                        timestamp_with_time_zone(People::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp_with_time_zone(People::ModifiedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_people_owner_id")
                            .from(People::Table, People::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_people_cover_photo_id")
                            .from(People::Table, People::CoverPhotoId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_people_owner_id")
                    .table(People::Table)
                    .col(People::OwnerId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 8. Create Faces Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Faces::Table)
                    .if_not_exists()
                    .col(char_len(Faces::Id, 21).primary_key())
                    .col(char_len(Faces::AssetId, 21))
                    .col(char_len_null(Faces::PersonId, 21))
                    .col(text(Faces::BoundingBox)) // JSON format
                    .col(ColumnDef::new(Faces::Embedding).binary_len(2048).null())
                    .col(double(Faces::Confidence))
                    .col(boolean(Faces::IsConfirmed).default(false))
                    .col(
                        timestamp_with_time_zone(Faces::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_faces_asset_id")
                            .from(Faces::Table, Faces::AssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_faces_person_id")
                            .from(Faces::Table, Faces::PersonId)
                            .to(People::Table, People::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_faces_asset_id")
                    .table(Faces::Table)
                    .col(Faces::AssetId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_faces_person_id")
                    .table(Faces::Table)
                    .col(Faces::PersonId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 9. Create Share Links Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(ShareLinks::Table)
                    .if_not_exists()
                    .col(char_len(ShareLinks::Id, 21).primary_key())
                    .col(string_len(ShareLinks::Token, 32).unique_key())
                    .col(char_len(ShareLinks::CreatorId, 21))
                    .col(string_len(ShareLinks::ShareType, 15))
                    .col(text(ShareLinks::TargetId))
                    .col(boolean(ShareLinks::AllowDownload).default(true))
                    .col(string_len_null(ShareLinks::PasswordHash, 255))
                    .col(timestamp_with_time_zone_null(ShareLinks::ExpiresAt))
                    .col(integer(ShareLinks::ViewCount).default(0))
                    .col(
                        timestamp_with_time_zone(ShareLinks::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_share_links_creator_id")
                            .from(ShareLinks::Table, ShareLinks::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_share_links_token")
                    .table(ShareLinks::Table)
                    .col(ShareLinks::Token)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_share_links_creator_id")
                    .table(ShareLinks::Table)
                    .col(ShareLinks::CreatorId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 10. Create Smart Tags Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(SmartTags::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SmartTags::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(string_len(SmartTags::Name, 100).unique_key())
                    .col(string_len(SmartTags::Category, 15))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_smart_tags_name")
                    .table(SmartTags::Table)
                    .col(SmartTags::Name)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 11. Create Asset Smart Tags Table (junction)
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(AssetSmartTags::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AssetSmartTags::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(char_len(AssetSmartTags::AssetId, 21))
                    .col(integer(AssetSmartTags::SmartTagId))
                    .col(double(AssetSmartTags::Confidence))
                    .col(
                        timestamp_with_time_zone(AssetSmartTags::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_smart_tags_asset_id")
                            .from(AssetSmartTags::Table, AssetSmartTags::AssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_smart_tags_smart_tag_id")
                            .from(AssetSmartTags::Table, AssetSmartTags::SmartTagId)
                            .to(SmartTags::Table, SmartTags::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("idx_asset_smart_tags_asset_tag_unique")
                            .table(AssetSmartTags::Table)
                            .col(AssetSmartTags::AssetId)
                            .col(AssetSmartTags::SmartTagId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_smart_tags_asset_id")
                    .table(AssetSmartTags::Table)
                    .col(AssetSmartTags::AssetId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_smart_tags_smart_tag_id")
                    .table(AssetSmartTags::Table)
                    .col(AssetSmartTags::SmartTagId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 12. Create Memories Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Memories::Table)
                    .if_not_exists()
                    .col(char_len(Memories::Id, 21).primary_key())
                    .col(char_len(Memories::OwnerId, 21))
                    .col(string_len(Memories::Title, 255))
                    .col(string_len_null(Memories::Subtitle, 255))
                    .col(char_len(Memories::CoverAssetId, 21))
                    .col(text(Memories::AssetIds)) // JSON array
                    .col(timestamp_with_time_zone(Memories::MemoryDate))
                    .col(boolean(Memories::IsSeen).default(false))
                    .col(boolean(Memories::IsHidden).default(false))
                    .col(
                        timestamp_with_time_zone(Memories::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_memories_owner_id")
                            .from(Memories::Table, Memories::OwnerId)
                            .to(Owners::Table, Owners::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_memories_cover_asset_id")
                            .from(Memories::Table, Memories::CoverAssetId)
                            .to(Assets::Table, Assets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_memories_owner_id")
                    .table(Memories::Table)
                    .col(Memories::OwnerId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_memories_memory_date")
                    .table(Memories::Table)
                    .col(Memories::MemoryDate)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;

        // ========================================
        // 13. Create Passkeys Table
        // ========================================
        manager
            .create_table(
                Table::create()
                    .table(Passkeys::Table)
                    .if_not_exists()
                    .col(char_len(Passkeys::Id, 21).primary_key())
                    .col(char_len(Passkeys::UserId, 21))
                    .col(binary(Passkeys::CredId))
                    .col(binary(Passkeys::PublicKey))
                    .col(big_integer(Passkeys::Counter))
                    .col(string(Passkeys::Name))
                    .col(
                        timestamp_with_time_zone(Passkeys::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(timestamp_with_time_zone_null(Passkeys::LastUsedAt))
                    .col(uuid_null(Passkeys::Aaguid))
                    .col(boolean(Passkeys::BackupEligible).default(false))
                    .col(boolean(Passkeys::BackupState).default(false))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_passkeys_user_id")
                            .from(Passkeys::Table, Passkeys::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_passkeys_user_id")
                    .table(Passkeys::Table)
                    .col(Passkeys::UserId)
                    .index_type(IndexType::Hash)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop tables in reverse order of creation (respecting foreign keys)
        manager
            .drop_table(Table::drop().table(Passkeys::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Memories::Table).to_owned())
            .await?;
        // Drop stack columns from assets first
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .drop_foreign_key(Alias::new("fk_assets_stack_id"))
                    .drop_column(Assets::StackId)
                    .drop_column(Assets::IsStackHidden)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(StackMembers::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AssetStacks::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AssetSmartTags::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SmartTags::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ShareLinks::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Faces::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(People::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Assets::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Friendships::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AlbumShares::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Albums::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OwnerMembers::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Owners::Table).to_owned())
            .await?;

        Ok(())
    }
}

// ========================================
// Table Identifiers
// ========================================

#[derive(DeriveIden)]
enum Owners {
    Table,
    Id,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Username,
    Name,
    Email,
    ProfileImageUrl,
    AccountVerified,
    NeedsOnboarding,
    PasswordHash,
    TotpSecret,
    TotpVerified,
    PasswordResetToken,
    PasswordResetExpiresAt,
    LastLoginAt,
    FailedLoginAttempts,
    IsAdmin,
    CreatedAt,
    ModifiedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum OwnerMembers {
    Table,
    Id,
    OwnerId,
    UserId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Albums {
    Table,
    Id,
    OwnerId,
    Name,
    Description,
    CreatedAt,
    ModifiedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum AlbumShares {
    Table,
    Id,
    AlbumId,
    UserId,
    Permission,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    Id,
    OwnerId,
    AlbumId,
    Width,
    Height,
    AssetType,
    OriginalFilename,
    FileSize,
    FileHash,
    ContentType,
    Latitude,
    Longitude,
    LqipHash,
    DominantColor,
    IsFavorite,
    CapturedAt,
    UploadedAt,
    ModifiedAt,
    DeletedAt,
    Uploaded,
    UploadUserId,
    StackId,
    IsStackHidden,
}

#[derive(DeriveIden)]
enum Friendships {
    Table,
    Id,
    UserId,
    FriendId,
    Status,
    CreatedAt,
    AcceptedAt,
}

#[derive(DeriveIden)]
enum People {
    Table,
    Id,
    OwnerId,
    Name,
    CoverPhotoId,
    IsHidden,
    FaceCount,
    CreatedAt,
    ModifiedAt,
}

#[derive(DeriveIden)]
enum Faces {
    Table,
    Id,
    AssetId,
    PersonId,
    BoundingBox,
    Embedding,
    Confidence,
    IsConfirmed,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ShareLinks {
    Table,
    Id,
    Token,
    CreatorId,
    ShareType,
    TargetId,
    AllowDownload,
    PasswordHash,
    ExpiresAt,
    ViewCount,
    CreatedAt,
}

#[derive(DeriveIden)]
enum SmartTags {
    Table,
    Id,
    Name,
    Category,
}

#[derive(DeriveIden)]
enum AssetSmartTags {
    Table,
    Id,
    AssetId,
    SmartTagId,
    Confidence,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Memories {
    Table,
    Id,
    OwnerId,
    Title,
    Subtitle,
    CoverAssetId,
    AssetIds,
    MemoryDate,
    IsSeen,
    IsHidden,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Passkeys {
    Table,
    Id,
    UserId,
    CredId,
    PublicKey,
    Counter,
    Name,
    CreatedAt,
    LastUsedAt,
    Aaguid,
    BackupEligible,
    BackupState,
}

#[derive(DeriveIden)]
enum AssetStacks {
    Table,
    Id,
    OwnerId,
    StackType,
    PrimaryAssetId,
    CoverAssetId,
    Metadata,
    IsCollapsed,
    IsAutoGenerated,
    CreatedAt,
    ModifiedAt,
}

#[derive(DeriveIden)]
enum StackMembers {
    Table,
    Id,
    StackId,
    AssetId,
    SequenceOrder,
    MemberRole,
    Metadata,
    CreatedAt,
}
