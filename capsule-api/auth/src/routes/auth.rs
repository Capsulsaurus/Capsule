use environment::constants::{
    RATE_LIMIT_LOGIN_MAX, RATE_LIMIT_LOGIN_WINDOW_SECS, RATE_LIMIT_REGISTER_MAX,
    RATE_LIMIT_REGISTER_WINDOW_SECS,
};
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use secrecy::ExposeSecret;

use crate::claims::Claims;
use crate::errors::ClaimValidationError;
use crate::models::requests::{LoginRequest, RefreshTokenRequest, RegisterRequest};
use crate::models::responses::{
    Device, GetDevicesResponses, LoginResponses, LogoutResponses, RefreshTokenResponses,
    RegisterUserResponses, ValidateTokenResponses,
};
use crate::state::AppState;
use crate::utils::headers::get_token_from_headers;

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

/// Register a new user
#[endpoint(operation_id = "register_user", tags("auth"))]
pub async fn register_user(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<RegisterRequest>,
) -> RegisterUserResponses {
    let state = depot.obtain::<AppState>().unwrap();

    // Per-IP rate limit
    let ip = get_client_ip(req);
    let rl_key = format!("register:{}", ip);
    match state
        .session_manager
        .check_rate_limit(
            &rl_key,
            RATE_LIMIT_REGISTER_MAX,
            RATE_LIMIT_REGISTER_WINDOW_SECS,
        )
        .await
    {
        Ok(result) if result.count > RATE_LIMIT_REGISTER_MAX => {
            return RegisterUserResponses::RateLimited(result.window_ttl_secs);
        }
        Err(e) => {
            tracing::warn!("Rate limit check failed: {}", e);
        }
        _ => {}
    }

    let request = body.into_inner();
    state
        .auth_service
        .register_user(&state.session_manager, request)
        .await
        .into()
}

/// Login a user
#[endpoint(operation_id = "login_user", tags("auth"))]
pub async fn login_user(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<LoginRequest>,
) -> LoginResponses {
    let state = depot.obtain::<AppState>().unwrap();

    // Per-IP rate limit
    let ip = get_client_ip(req);
    let rl_key = format!("login:{}", ip);
    match state
        .session_manager
        .check_rate_limit(&rl_key, RATE_LIMIT_LOGIN_MAX, RATE_LIMIT_LOGIN_WINDOW_SECS)
        .await
    {
        Ok(result) if result.count > RATE_LIMIT_LOGIN_MAX => {
            return LoginResponses::RateLimited(result.window_ttl_secs);
        }
        Err(e) => {
            tracing::warn!("Rate limit check failed: {}", e);
        }
        _ => {}
    }

    let LoginRequest { email, password } = body.into_inner();
    state
        .auth_service
        .authenticate_user(&state.session_manager, &email, &password)
        .await
        .into()
}

/// Refresh an access token using a refresh token
#[endpoint(operation_id = "refresh_token", tags("auth"))]
pub async fn refresh_token(
    depot: &mut Depot,
    body: JsonBody<RefreshTokenRequest>,
) -> RefreshTokenResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let payload = body.into_inner();

    let token = match Claims::decode(
        payload.refresh_token.expose_secret(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(token) => token,
        Err(e) => {
            return ClaimValidationError::from(e).into();
        }
    };

    let sid = match token.claims.sid {
        Some(sid) => sid,
        None => {
            return ClaimValidationError::TokenInvalid("Missing session ID".to_string()).into();
        }
    };

    // Validate session
    let session = match state.session_manager.get_session(&sid).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return ClaimValidationError::TokenInvalid("Session not found".to_string()).into();
        }
        Err(e) => return e.into(),
    };

    // Verify user ownership
    if session.user_id != token.claims.sub {
        return ClaimValidationError::TokenInvalid("Session user mismatch".to_string()).into();
    }

    // Revoke old session (Rotation)
    if let Err(e) = state.session_manager.revoke_session(&sid).await {
        return e.into();
    }

    let user_id = token.claims.sub;

    state
        .auth_service
        .generate_token_pair(&user_id, &state.session_manager)
        .await
        .into()
}

/// Validate an access token
#[endpoint(operation_id = "validate_token", tags("auth"), security(("bearer" = [])))]
pub async fn validate_token(req: &mut Request, depot: &mut Depot) -> ValidateTokenResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let headers = req.headers();

    // Get token string
    let token_string = match get_token_from_headers(headers) {
        Ok(token_string) => token_string,
        Err(e) => return e.into(),
    };

    // Validate token
    let claims = match state.auth_service.get_claims(token_string.expose_secret()) {
        Ok(claims) => claims,
        Err(e) => return e.into(),
    };

    if let Err(e) = claims.validate_access_token() {
        return e.into();
    }

    Ok(claims).into()
}

/// Logout user and invalidate tokens
#[endpoint(operation_id = "logout", tags("auth"), security(("bearer" = [])))]
pub async fn logout(req: &mut Request, depot: &mut Depot) -> LogoutResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let headers = req.headers();

    // Authorize user
    let token_string = match get_token_from_headers(headers) {
        Ok(token_string) => token_string,
        Err(e) => return e.into(),
    };

    // Just validate the token signatures
    let claims = match state.auth_service.get_claims(token_string.expose_secret()) {
        Ok(claims) => claims,
        Err(e) => return e.into(),
    };

    if let Err(e) = claims.validate_access_token() {
        return e.into();
    }

    // If it has a session ID, revoke it
    if let Some(sid) = claims.sid
        && let Err(e) = state.session_manager.revoke_session(&sid).await
    {
        return e.into();
    }

    LogoutResponses::Success
}

/// Get all active devices (sessions)
#[endpoint(operation_id = "get_devices", tags("auth"), security(("bearer" = [])))]
pub async fn get_devices(req: &mut Request, depot: &mut Depot) -> GetDevicesResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let headers = req.headers();

    let token_string = match get_token_from_headers(headers) {
        Ok(token_string) => token_string,
        Err(e) => return e.into(),
    };

    let claims = match state.auth_service.get_claims(token_string.expose_secret()) {
        Ok(claims) => claims,
        Err(e) => return e.into(),
    };

    if let Err(e) = claims.validate_access_token() {
        return e.into();
    }

    let current_sid = claims.sid.clone();

    let sessions = match state
        .session_manager
        .get_sessions_for_user(&claims.sub)
        .await
    {
        Ok(sessions) => sessions,
        Err(e) => return e.into(),
    };

    let devices: Vec<Device> = sessions
        .into_iter()
        .map(|(sid, session)| {
            let is_current = current_sid.as_deref() == Some(sid.as_str());
            let last_active_at = if session.last_active_at == 0 {
                session.created_at
            } else {
                session.last_active_at
            };
            Device {
                id: sid,
                created_at: session.created_at,
                last_active_at,
                user_agent: session.user_agent,
                ip_address: session.ip_address,
                is_current,
            }
        })
        .collect();

    GetDevicesResponses::Success(devices)
}
