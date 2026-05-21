use environment::constants::{
    RATE_LIMIT_PASSWORD_RESET_MAX, RATE_LIMIT_PASSWORD_RESET_WINDOW_SECS,
};
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use service::user as UserService;
use tracing::trace;

use crate::models::responses::{PasswordResetResponses, ResetPasswordRequestResponses};
use crate::models::{ResetPasswordPayload, ResetPasswordRequestPayload};
use crate::state::AppState;
use crate::utils::hash::hash_password;

fn get_client_ip(req: &Request) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            req.headers()
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Request password reset
#[endpoint(operation_id = "reset_password_request", tags("auth"))]
pub async fn reset_password_request(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<ResetPasswordRequestPayload>,
) -> ResetPasswordRequestResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let payload = body.into_inner();
    let email = payload.email;

    // Per-email rate limit
    let rl_key = format!("pwd_reset:{}", email.to_lowercase());
    match state
        .session_manager
        .check_rate_limit(
            &rl_key,
            RATE_LIMIT_PASSWORD_RESET_MAX,
            RATE_LIMIT_PASSWORD_RESET_WINDOW_SECS,
        )
        .await
    {
        Ok(result) if result.count > RATE_LIMIT_PASSWORD_RESET_MAX => {
            return ResetPasswordRequestResponses::RateLimited(result.window_ttl_secs);
        }
        Err(e) => {
            tracing::warn!("Rate limit check failed: {}", e);
        }
        _ => {}
    }

    // Per-IP rate limit as secondary guard
    let ip = get_client_ip(req);
    let ip_rl_key = format!("pwd_reset_ip:{}", ip);
    match state
        .session_manager
        .check_rate_limit(
            &ip_rl_key,
            RATE_LIMIT_PASSWORD_RESET_MAX,
            RATE_LIMIT_PASSWORD_RESET_WINDOW_SECS,
        )
        .await
    {
        Ok(result) if result.count > RATE_LIMIT_PASSWORD_RESET_MAX => {
            return ResetPasswordRequestResponses::RateLimited(result.window_ttl_secs);
        }
        Err(e) => {
            tracing::warn!("Rate limit check failed: {}", e);
        }
        _ => {}
    }

    if let Err(e) = state
        .password_service
        .request_reset(&state.conn, &email)
        .await
    {
        return ResetPasswordRequestResponses::InternalServerError(eyre::eyre!(e).into());
    }

    ResetPasswordRequestResponses::Success
}

/// Reset password with token
#[endpoint(operation_id = "reset_password", tags("auth"))]
pub async fn reset_password(
    depot: &mut Depot,
    body: JsonBody<ResetPasswordPayload>,
) -> PasswordResetResponses {
    let state = depot.obtain::<AppState>().unwrap();

    let ResetPasswordPayload {
        token,
        new_password,
    } = body.into_inner();

    // Find user by token
    let user = match UserService::Query::find_user_by_reset_token(&state.conn, &token).await {
        Ok(user) => user,
        Err(e) => return PasswordResetResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    let user = match user {
        Some(user) => user,
        None => return PasswordResetResponses::InvalidToken,
    };

    // Check expiry
    if user
        .password_reset_expires_at
        .is_none_or(|exp| exp < chrono::Utc::now())
    {
        return PasswordResetResponses::InvalidToken;
    }

    // Validate new password strength
    if !UserService::is_valid_password(&new_password) {
        trace!("Invalid password during reset for user {}", user.id);
        return PasswordResetResponses::InvalidNewPassword;
    }

    // Hash password
    let password_hash = match hash_password(&new_password) {
        Ok(hash) => hash,
        Err(e) => return PasswordResetResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    // Confirm reset
    if let Err(e) =
        UserService::Mutation::confirm_password_reset(&state.conn, &user.id, &password_hash).await
    {
        return PasswordResetResponses::InternalServerError(eyre::eyre!(e).into());
    }

    // Revoke all existing sessions for security
    if let Err(e) = state.session_manager.revoke_all_for_user(&user.id).await {
        tracing::error!(
            "Failed to revoke sessions after password reset for user {}: {}",
            user.id,
            e
        );
    }

    PasswordResetResponses::Success
}
