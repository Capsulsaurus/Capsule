use std::time::Duration;

use salvo::http::cookie::{Cookie, SameSite};
use salvo::prelude::*;

use crate::models::responses::*;
use crate::session::SessionContext;
use crate::state::AppState;
use crate::utils::headers::validate_user_from_headers;

/// The passkey-assertion body, split into the ceremony's client-asserted session provenance
/// and the WebAuthn credential itself (slice `S-N3`).
///
/// A passkey login is a login ceremony like any other, so it carries the same optional
/// `cohort_hash`/`device_id` password login does — without them a passkey session lands in
/// the devices view as an unknown, ungrouped device. Both are pulled out *beside* the
/// credential (the credential's own fields are flattened through untouched), so an older
/// client that posts a bare credential keeps working unchanged.
#[derive(serde::Deserialize)]
struct FinishAuthBody {
    /// Advisory device-cohort hash (slice `S-C13`); grouping only, never authorization.
    #[serde(default)]
    cohort_hash: Option<String>,
    /// The asserted directory `device_id`; surfaced on the session listing only.
    #[serde(default)]
    device_id: Option<String>,
    /// The WebAuthn assertion, untouched.
    #[serde(flatten)]
    credential: serde_json::Value,
}

impl FinishAuthBody {
    /// Split the parsed body into the WebAuthn credential and the session provenance to
    /// attach to the session this ceremony opens.
    fn split(
        self,
    ) -> Result<(webauthn_rs::prelude::PublicKeyCredential, SessionContext), serde_json::Error>
    {
        let context = SessionContext::new(self.cohort_hash, self.device_id);
        let credential = serde_json::from_value(self.credential)?;
        Ok((credential, context))
    }
}

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
                    &format!("passkey_reg:{challenge_id}"),
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
        .get_temp_data(&format!("passkey_reg:{challenge_id}"))
        .await
        .map_err(PasskeyRegistrationFinishResponses::InternalServerError)?
        .ok_or(PasskeyRegistrationFinishResponses::RegistrationFailed(
            "Registration session expired".into(),
        ))?;

    // Clear state
    let _ = state
        .session_manager
        .delete_temp_data(&format!("passkey_reg:{challenge_id}"))
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
                    &format!("passkey_auth:{challenge_id}"),
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

    // Parse body manually: the credential plus the ceremony's optional session provenance.
    let body = req
        .parse_json::<FinishAuthBody>()
        .await
        .map_err(|_| PasskeyAuthFinishResponses::InvalidCredential)?;
    let (cred, context) = body
        .split()
        .map_err(|_| PasskeyAuthFinishResponses::InvalidCredential)?;

    // Retrieve state
    let auth_state: webauthn_rs::prelude::PasskeyAuthentication = state
        .session_manager
        .get_temp_data(&format!("passkey_auth:{challenge_id}"))
        .await
        .map_err(PasskeyAuthFinishResponses::InternalServerError)?
        .ok_or(PasskeyAuthFinishResponses::InvalidCredential)?;

    // Clear state
    let _ = state
        .session_manager
        .delete_temp_data(&format!("passkey_auth:{challenge_id}"))
        .await;

    let user_id = state
        .passkey_service
        .finish_authentication(auth_state, cred)
        .await?;

    // Issue new tokens, carrying the ceremony's asserted cohort/device so a passkey login
    // groups in the devices view exactly as a password login does (slice `S-N3`).
    let tokens = state
        .auth_service
        .generate_token_pair(&user_id, &state.session_manager, context)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a WebAuthn assertion body, minus the (opaque, base64) crypto material —
    /// enough to prove the `#[serde(flatten)]` split does not disturb the credential.
    fn assertion_fields() -> serde_json::Value {
        serde_json::json!({
            "id": "Y3JlZC1pZA",
            "rawId": "Y3JlZC1pZA",
            "type": "public-key",
            "response": { "clientDataJSON": "e30", "authenticatorData": "AA", "signature": "AA" },
        })
    }

    #[test]
    fn assertion_body_splits_provenance_from_the_credential() {
        let mut body = assertion_fields();
        body["cohort_hash"] = serde_json::Value::String("a".repeat(64));
        body["device_id"] = serde_json::json!("1F2E3D4C-5B6A-4978-8899-AABBCCDDEEFF");

        let parsed: FinishAuthBody = serde_json::from_value(body).expect("body parses");
        assert_eq!(parsed.cohort_hash, Some("a".repeat(64)));
        assert_eq!(
            parsed.device_id.as_deref(),
            Some("1F2E3D4C-5B6A-4978-8899-AABBCCDDEEFF")
        );

        // The credential half keeps every WebAuthn field and gains neither of ours — the
        // assertion must reach `webauthn-rs` byte-identical to what the client sent.
        let credential = parsed.credential.as_object().expect("credential object");
        for field in ["id", "rawId", "type", "response"] {
            assert!(credential.contains_key(field), "{field} survived the split");
        }
        assert!(!credential.contains_key("cohort_hash"));
        assert!(!credential.contains_key("device_id"));
    }

    #[test]
    fn assertion_body_without_provenance_still_parses() {
        // An older client posts a bare credential: the ceremony still runs, the session just
        // carries no grouping metadata.
        let parsed: FinishAuthBody =
            serde_json::from_value(assertion_fields()).expect("bare credential parses");
        assert_eq!(parsed.cohort_hash, None);
        assert_eq!(parsed.device_id, None);

        let context = SessionContext::new(parsed.cohort_hash, parsed.device_id);
        assert_eq!(context, SessionContext::default());
    }

    #[test]
    fn assertion_body_normalizes_provenance_like_every_other_ceremony() {
        let mut body = assertion_fields();
        body["cohort_hash"] = serde_json::json!("   ");
        body["device_id"] = serde_json::json!("not-a-uuid");

        let parsed: FinishAuthBody = serde_json::from_value(body).expect("body parses");
        let context = SessionContext::new(parsed.cohort_hash, parsed.device_id).normalized();
        assert_eq!(
            context,
            SessionContext::default(),
            "garbage provenance is indistinguishable from none"
        );
    }
}
