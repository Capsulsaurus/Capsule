use model::user::User;
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use secrecy::ExposeSecret;
use service::user as UserService;

use crate::models::UserProfile;
use crate::models::requests::UpdateProfileRequest;
use crate::models::responses::{UpdateUserProfileResponses, UserProfileResponses};
use crate::state::AppState;
use crate::utils::hash::{hash_password, verify_password};

/// Get user profile
#[endpoint(operation_id = "get_user_profile", tags("auth"), security(("bearer" = [])))]
pub async fn get_user_profile(req: &mut Request, depot: &mut Depot) -> UserProfileResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let headers = req.headers();

    // Authorize user
    let user_id = match crate::utils::headers::validate_user_from_headers(
        headers,
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(user_id) => user_id,
        Err(e) => return UserProfileResponses::Unauthorized(e),
    };

    // Fetch user profile from database
    let user = match UserService::Query::find_user_by_id(&state.conn, &user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return UserProfileResponses::UserNotFound,
        Err(e) => return UserProfileResponses::InternalServerError(eyre::eyre!(e).into()),
    };
    let profile = UserProfile::from(user);

    UserProfileResponses::Success(profile)
}

/// Update user profile
#[endpoint(operation_id = "update_user_profile", tags("auth"), security(("bearer" = [])))]
pub async fn update_user_profile(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<UpdateProfileRequest>,
) -> UpdateUserProfileResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let headers = req.headers();

    // Authorize user
    let user_id = match crate::utils::headers::validate_user_from_headers(
        headers,
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(user_id) => user_id,
        Err(e) => return UpdateUserProfileResponses::Unauthorized(e),
    };

    // Parse request body
    let payload = body.into_inner();

    // Fetch current user to check password if needed
    let _current_user = match UserService::Query::find_user_by_id(&state.conn, &user_id.clone())
        .await
    {
        Ok(Some(user)) => user,
        Ok(None) => return UpdateUserProfileResponses::UserNotFound,
        Err(e) => return UpdateUserProfileResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    let current_password_hash =
        match UserService::Query::get_password_hash_by_id(&state.conn, &user_id).await {
            Ok(Some(hash)) => hash,
            Ok(None) => return UpdateUserProfileResponses::UserNotFound,
            Err(e) => {
                return UpdateUserProfileResponses::InternalServerError(eyre::eyre!(e).into());
            }
        };

    // Handle password change
    let new_password_hash = if let (Some(current_password), Some(new_password)) = (
        payload.current_password.as_ref(),
        payload.new_password.as_ref(),
    ) {
        match verify_password(current_password.expose_secret(), &current_password_hash) {
            Ok(true) => match hash_password(new_password.expose_secret()) {
                Ok(hash) => Some(hash),
                Err(e) => {
                    return UpdateUserProfileResponses::InternalServerError(eyre::eyre!(e).into());
                }
            },
            Ok(false) => return UpdateUserProfileResponses::InvalidPassword,
            Err(e) => {
                return UpdateUserProfileResponses::InternalServerError(eyre::eyre!(e).into());
            }
        }
    } else {
        None
    };

    // Update user profile in database
    let updated_user = match UserService::Mutation::update_user(
        &state.conn,
        &user_id,
        service::user::UpdateUserArgs {
            username: payload.username,
            name: None, // Name update not exposed in request yet?
            email: payload.email,
            password_hash: new_password_hash,
        },
    )
    .await
    {
        Ok(user) => user,
        Err(sea_orm::DbErr::RecordNotFound(_)) => return UpdateUserProfileResponses::UserNotFound,
        Err(e) => return UpdateUserProfileResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    let updated_user = User::from(updated_user);
    let updated_profile = UserProfile::from(updated_user);

    UpdateUserProfileResponses::Success(updated_profile)
}
