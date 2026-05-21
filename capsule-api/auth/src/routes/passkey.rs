use crate::models::responses::*;
use crate::state::AppState;
use crate::utils::headers::validate_user_from_headers;
use salvo::http::cookie::{Cookie, SameSite};
use salvo::prelude::*;

use std::time::Duration;

#[handler]
pub async fn start_registration(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<PasskeyRegistrationStartResponses, PasskeyRegistrationStartResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyRegistrationStartResponses::InternalServerError(
            eyre::eyre!("State not found").into(),
        )
    })?;

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => {
                return Err(PasskeyRegistrationStartResponses::Unauthorized(
                    e.to_string(),
                ));
            }
        };

    let user = service::user::Query::find_user_by_id(&state.conn, &user_id)
        .await
        .map_err(|e| PasskeyRegistrationStartResponses::InternalServerError(e.into()))?
        .ok_or(PasskeyRegistrationStartResponses::UserNotFound)?;

    let passkey_res = state
        .passkey_service
        .start_registration(user.id, user.username.clone(), user.name)
        .await;

    match passkey_res {
        Ok((ccr, reg_state)) => {
            // Store registration state in temporary storage
            let challenge_id = nanoid::nanoid!();
            state
                .session_manager
                .save_temp_data(
                    &format!("passkey_reg:{}", challenge_id),
                    &reg_state,
                    Duration::from_secs(300), // 5 minutes
                )
                .await
                .map_err(PasskeyRegistrationStartResponses::InternalServerError)?;

            // Set cookie for challenge ID
            let cookie = Cookie::build(("passkey_reg_id", challenge_id))
                .path("/")
                .http_only(true)
                .secure(true) // Ensure secure in prod
                .same_site(SameSite::Lax) // Lax for basic flow
                .build();
            res.add_cookie(cookie);

            Ok(PasskeyRegistrationStartResponses::Success(ccr))
        }
        Err(e) => Err(e.into()),
    }
}

#[handler]
pub async fn finish_registration(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<PasskeyRegistrationFinishResponses, PasskeyRegistrationFinishResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyRegistrationFinishResponses::InternalServerError(
            eyre::eyre!("State not found").into(),
        )
    })?;

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => {
                return Err(PasskeyRegistrationFinishResponses::Unauthorized(
                    e.to_string(),
                ));
            }
        };

    // Get challenge ID from cookie
    let challenge_id = req
        .cookie("passkey_reg_id")
        .map(|c| c.value().to_string())
        .ok_or(PasskeyRegistrationFinishResponses::RegistrationFailed(
            "Missing registration session".into(),
        ))?;

    // Parse body: credential fields + optional `name`
    #[derive(serde::Deserialize)]
    struct FinishRegBody {
        name: Option<String>,
        #[serde(flatten)]
        rest: serde_json::Value,
    }
    let parsed = req
        .parse_json::<FinishRegBody>()
        .await
        .map_err(|e| PasskeyRegistrationFinishResponses::RegistrationFailed(e.to_string()))?;
    let passkey_name = parsed.name.unwrap_or_else(|| "My Passkey".to_string());
    let reg: webauthn_rs::prelude::RegisterPublicKeyCredential =
        serde_json::from_value(parsed.rest)
            .map_err(|e| PasskeyRegistrationFinishResponses::RegistrationFailed(e.to_string()))?;

    // Retrieve state
    let reg_state: webauthn_rs::prelude::PasskeyRegistration = state
        .session_manager
        .get_temp_data(&format!("passkey_reg:{}", challenge_id))
        .await
        .map_err(PasskeyRegistrationFinishResponses::InternalServerError)?
        .ok_or(PasskeyRegistrationFinishResponses::RegistrationFailed(
            "Registration session expired".into(),
        ))?;

    // Clear state
    let _ = state
        .session_manager
        .delete_temp_data(&format!("passkey_reg:{}", challenge_id))
        .await;

    state
        .passkey_service
        .finish_registration(user_id, reg_state, reg, passkey_name)
        .await?;

    Ok(PasskeyRegistrationFinishResponses::Success)
}

#[handler]
pub async fn start_authentication(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<PasskeyAuthStartResponses, PasskeyAuthStartResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyAuthStartResponses::InternalServerError(eyre::eyre!("State not found").into())
    })?;

    #[derive(serde::Deserialize)]
    struct AuthStartRequest {
        username: Option<String>,
    }

    let username = req
        .parse_json::<AuthStartRequest>()
        .await
        .ok()
        .and_then(|r| r.username);

    let passkey_res = state.passkey_service.start_authentication(username).await;
    match passkey_res {
        Ok((rcr, auth_state)) => {
            // Store auth state in temporary storage
            let challenge_id = nanoid::nanoid!();
            state
                .session_manager
                .save_temp_data(
                    &format!("passkey_auth:{}", challenge_id),
                    &auth_state,
                    Duration::from_secs(300),
                )
                .await
                .map_err(PasskeyAuthStartResponses::InternalServerError)?;

            // Set cookie
            let cookie = Cookie::build(("passkey_auth_id", challenge_id))
                .path("/")
                .http_only(true)
                .secure(true)
                .same_site(SameSite::Lax)
                .build();
            res.add_cookie(cookie);

            Ok(PasskeyAuthStartResponses::Success(rcr))
        }
        Err(e) => Err(e.into()),
    }
}

#[handler]
pub async fn finish_authentication(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<PasskeyAuthFinishResponses, PasskeyAuthFinishResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyAuthFinishResponses::InternalServerError(eyre::eyre!("State not found").into())
    })?;

    // Get challenge ID from cookie
    let challenge_id = req
        .cookie("passkey_auth_id")
        .map(|c| c.value().to_string())
        .ok_or(PasskeyAuthFinishResponses::InvalidCredential)?;

    // Parse body manually
    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .map_err(|_| PasskeyAuthFinishResponses::InvalidCredential)?;
    let cred: webauthn_rs::prelude::PublicKeyCredential =
        serde_json::from_value(body).map_err(|_| PasskeyAuthFinishResponses::InvalidCredential)?;

    // Retrieve state
    let auth_state: webauthn_rs::prelude::PasskeyAuthentication = state
        .session_manager
        .get_temp_data(&format!("passkey_auth:{}", challenge_id))
        .await
        .map_err(PasskeyAuthFinishResponses::InternalServerError)?
        .ok_or(PasskeyAuthFinishResponses::InvalidCredential)?;

    // Clear state
    let _ = state
        .session_manager
        .delete_temp_data(&format!("passkey_auth:{}", challenge_id))
        .await;

    let user_id = state
        .passkey_service
        .finish_authentication(auth_state, cred)
        .await?;

    // Issue new tokens (Renamed from generate_tokens to generate_token_pair)
    let tokens = state
        .auth_service
        .generate_token_pair(&user_id, &state.session_manager)
        .await
        .map_err(PasskeyAuthFinishResponses::InternalServerError)?;

    Ok(PasskeyAuthFinishResponses::Success(tokens))
}

// Management
#[handler]
pub async fn list_credentials(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<PasskeyListResponses, PasskeyListResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyListResponses::InternalServerError(eyre::eyre!("State not found").into())
    })?;
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return Err(PasskeyListResponses::Unauthorized(e.to_string())),
        };

    let credentials = state.passkey_service.list_credentials(user_id).await?;
    Ok(PasskeyListResponses::Success(
        credentials.into_iter().map(Into::into).collect(),
    ))
}

#[handler]
pub async fn delete_credential(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<PasskeyManageResponses, PasskeyManageResponses> {
    let state = depot.obtain::<AppState>().map_err(|_| {
        PasskeyManageResponses::InternalServerError(eyre::eyre!("State not found").into())
    })?;
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return Err(PasskeyManageResponses::Unauthorized(e.to_string())),
        };

    let cred_id = req.param::<String>("cred_id").unwrap_or_default();

    state
        .passkey_service
        .delete_credential(user_id, cred_id)
        .await?;

    Ok(PasskeyManageResponses::Success)
}
