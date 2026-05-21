use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;

use crate::claims::{Claims, Scope};
use crate::models::requests::{
    DisableTotpRequest, VerifyTotpEnrollmentRequest, VerifyTotpLoginRequest,
};
use crate::models::responses::{
    TotpDisableResponses, TotpEnrollResponses, TotpEnrollmentResponse,
    TotpVerifyEnrollmentResponses, TotpVerifyLoginResponses,
};
use crate::state::AppState;

use secrecy::ExposeSecret;

/// Enroll in TOTP - generates secret and provisioning URI
#[endpoint(operation_id = "totp_enroll", tags("totp"), security(("bearer" = [])))]
pub async fn totp_enroll(req: &mut Request, depot: &mut Depot) -> TotpEnrollResponses {
    let state = depot.obtain::<AppState>().unwrap();

    // Get authenticated user from token
    let user_id = match crate::utils::headers::validate_user_from_headers(
        req.headers(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(id) => id,
        Err(e) => return e.into(),
    };

    let user_id = &user_id;

    match state.totp_service.enroll(user_id).await {
        Ok(provisioning_uri) => {
            return TotpEnrollResponses::Success(TotpEnrollmentResponse { provisioning_uri });
        }
        Err(e) => return e.into(),
    }
}

/// Verify TOTP enrollment - confirms the secret works
#[endpoint(operation_id = "totp_verify_enrollment", tags("totp"), security(("bearer" = [])))]
pub async fn totp_verify_enrollment(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<VerifyTotpEnrollmentRequest>,
) -> TotpVerifyEnrollmentResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let request = body.into_inner();

    // Get authenticated user
    let user_id = match crate::utils::headers::validate_user_from_headers(
        req.headers(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(id) => id,
        Err(e) => return TotpVerifyEnrollmentResponses::Unauthorized(e),
    };

    let user_id = &user_id;

    // Verify enrollment
    match state
        .totp_service
        .verify_enrollment(user_id, &request.totp_code)
        .await
    {
        Ok(true) => {
            return TotpVerifyEnrollmentResponses::Success;
        }
        Ok(false) => {
            return TotpVerifyEnrollmentResponses::InvalidCode;
        }
        Err(e) => return e.into(),
    }
}

/// Disable TOTP for the authenticated user
#[endpoint(operation_id = "totp_disable", tags("totp"), security(("bearer" = [])))]
pub async fn totp_disable(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<DisableTotpRequest>,
) -> TotpDisableResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let request = body.into_inner();
    let DisableTotpRequest { totp_code } = request;

    // Get authenticated user
    let user_id = match crate::utils::headers::validate_user_from_headers(
        req.headers(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(id) => id,
        Err(e) => return TotpDisableResponses::Unauthorized(e),
    };

    state
        .totp_service
        .clear_enrollment(&user_id, &totp_code)
        .await
        .into()
}

/// Verify TOTP code during login and return access tokens
#[endpoint(operation_id = "totp_verify_login", tags("totp"))]
pub async fn totp_verify_login(
    depot: &mut Depot,
    body: JsonBody<VerifyTotpLoginRequest>,
) -> TotpVerifyLoginResponses {
    let state = depot.obtain::<AppState>().unwrap();
    let request = body.into_inner();

    // Decode MFA token
    let mfa_claims = match Claims::decode(
        request.mfa_token.expose_secret(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(token_data) => token_data.claims,
        Err(_e) => return TotpVerifyLoginResponses::InvalidMfaToken,
    };

    // Check if MFA token is still valid
    if !mfa_claims.is_valid() {
        return TotpVerifyLoginResponses::InvalidMfaToken;
    }

    // Check MFA token has the MFA scope (security check)
    if !mfa_claims.has_scopes(&[Scope::MfaToken]) {
        return TotpVerifyLoginResponses::InvalidMfaToken;
    }

    let user_id = &mfa_claims.sub;
    let jti = &mfa_claims.jti;

    // Check attempt count
    let attempts = match state.session_manager.get_mfa_attempts(jti).await {
        Ok(count) => count,
        Err(e) => return TotpVerifyLoginResponses::InternalServerError(e),
    };

    if attempts >= 3 {
        return TotpVerifyLoginResponses::MaxAttemptsExceeded;
    }

    // Verify TOTP code
    let is_valid: bool = match state
        .totp_service
        .verify_enrollment(user_id, &request.totp_code)
        .await
    {
        Ok(v) => v,
        Err(e) => return e.into(),
    };

    if !is_valid {
        // Increment attempt counter
        if let Err(e) = state.session_manager.increment_mfa_attempt(jti).await {
            return TotpVerifyLoginResponses::InternalServerError(e);
        }
        return TotpVerifyLoginResponses::InvalidCode;
    }

    // Clear MFA attempts on success
    if let Err(e) = state.session_manager.clear_mfa_attempts(jti).await {
        return TotpVerifyLoginResponses::InternalServerError(e);
    }

    // Generate full token pair
    match state
        .auth_service
        .generate_token_pair(user_id, &state.session_manager)
        .await
    {
        Ok(tokens) => TotpVerifyLoginResponses::Success(tokens),
        Err(e) => TotpVerifyLoginResponses::InternalServerError(e),
    }
}
