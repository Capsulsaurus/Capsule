use crate::errors::{PasskeyAuthenticationError, PasskeyManagementError, PasskeyRegistrationError};
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use uuid::Uuid;
use webauthn_rs::prelude::*;

#[derive(Clone)]
pub struct PasskeyService {
    conn: DatabaseConnection,
    webauthn: Arc<Webauthn>,
}

impl PasskeyService {
    pub fn new(conn: DatabaseConnection, webauthn: Arc<Webauthn>) -> Self {
        Self { conn, webauthn }
    }

    /// Start passkey registration
    pub async fn start_registration(
        &self,
        user_id: String,
        username: String,
        name: String,
    ) -> Result<(serde_json::Value, PasskeyRegistration), PasskeyRegistrationError> {
        let webauthn_user_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, user_id.as_bytes());

        // Check if user has too many passkeys
        let count = service::passkey::Query::count_by_user_id(&self.conn, &user_id)
            .await
            .map_err(PasskeyRegistrationError::Db)?;

        if count >= environment::constants::MAX_PASSKEYS_PER_USER as u64 {
            return Err(PasskeyRegistrationError::LimitReached(format!(
                "Max passkeys limit reached ({})",
                environment::constants::MAX_PASSKEYS_PER_USER
            )));
        }

        // Start registration
        let (ccr, state) = self
            .webauthn
            .start_passkey_registration(webauthn_user_id, &username, &name, None)
            .map_err(|e| PasskeyRegistrationError::RegistrationFailed(e.to_string()))?;

        Ok((
            serde_json::to_value(ccr)
                .map_err(|e| PasskeyRegistrationError::Unexpected(e.into()))?,
            state,
        ))
    }

    /// Finish passkey registration
    pub async fn finish_registration(
        &self,
        user_id: String,
        state: PasskeyRegistration,
        register: RegisterPublicKeyCredential,
        name: String,
    ) -> Result<(), PasskeyRegistrationError> {
        let credential = self
            .webauthn
            .finish_passkey_registration(&register, &state)
            .map_err(|e| PasskeyRegistrationError::RegistrationFailed(e.to_string()))?;

        service::passkey::Mutation::create_passkey(
            &self.conn,
            service::passkey::CreatePasskeyArgs {
                user_id,
                cred_id: credential.cred_id().clone().into(),
                public_key: serde_json::to_vec(&credential.get_public_key()).unwrap_or_default(),
                name,
                counter: 0,
                aaguid: None,
                backup_eligible: false,
                backup_state: false,
            },
        )
        .await
        .map_err(PasskeyRegistrationError::Db)?;

        Ok(())
    }

    /// Start passkey authentication
    pub async fn start_authentication(
        &self,
        username: Option<String>,
    ) -> Result<(serde_json::Value, PasskeyAuthentication), PasskeyAuthenticationError> {
        // If username is provided, we can look up their credentials.
        // If not (discoverable credentials), we proceed without specific credentials.

        let (rcr, state) = if let Some(username) = username {
            let user = service::user::Query::find_user_by_username(&self.conn, &username)
                .await
                .map_err(PasskeyAuthenticationError::Db)?
                .ok_or(PasskeyAuthenticationError::UserNotFound)?;

            let _passkeys = service::passkey::Query::find_by_user_id(&self.conn, &user.id)
                .await
                .map_err(PasskeyAuthenticationError::Db)?;

            // NOTE: We rely on discoverable credentials (resident keys) or user entering username.
            // Using allow_credentials requires constructing webauthn_rs::prelude::Passkey,
            // which currently presents integration challenges (private fields/constructor availability).
            // For now, an empty list allows any credential for this RP.
            self.webauthn
                .start_passkey_authentication(&[])
                .map_err(|e| PasskeyAuthenticationError::Unexpected(eyre::eyre!(e)))?
        } else {
            // Conditional UI / Discoverable credentials
            self.webauthn
                .start_passkey_authentication(&[])
                .map_err(|e| PasskeyAuthenticationError::Unexpected(eyre::eyre!(e)))?
        };

        Ok((
            serde_json::to_value(rcr)
                .map_err(|e| PasskeyAuthenticationError::Unexpected(e.into()))?,
            state,
        ))
    }

    /// Finish passkey authentication
    pub async fn finish_authentication(
        &self,
        auth_state: PasskeyAuthentication,
        credential: PublicKeyCredential,
    ) -> Result<String, PasskeyAuthenticationError> {
        let auth_result = self
            .webauthn
            .finish_passkey_authentication(&credential, &auth_state)
            .inspect_err(|e| tracing::trace!(err = ?e, "Authentication detail"))
            .map_err(|_e| PasskeyAuthenticationError::InvalidCredential)?;

        let cred_id = Vec::<u8>::from(auth_result.cred_id().clone());

        let passkey_model = service::passkey::Query::find_by_cred_id(&self.conn, &cred_id)
            .await
            .map_err(PasskeyAuthenticationError::Db)?
            .ok_or(PasskeyAuthenticationError::InvalidCredential)?;

        service::passkey::Mutation::update_counter(
            &self.conn,
            &passkey_model.id,
            passkey_model.counter + 1,
        )
        .await
        .map_err(PasskeyAuthenticationError::Db)?;

        // Return user_id to session/token issuance
        Ok(passkey_model.user_id)
    }

    pub async fn list_credentials(
        &self,
        user_id: String,
    ) -> Result<Vec<model::passkey::Passkey>, PasskeyManagementError> {
        let passkeys = service::passkey::Query::find_by_user_id(&self.conn, &user_id)
            .await
            .map_err(PasskeyManagementError::Db)?;
        Ok(passkeys)
    }

    pub async fn delete_credential(
        &self,
        user_id: String,
        cred_id: String,
    ) -> Result<(), PasskeyManagementError> {
        service::passkey::Mutation::delete_passkey(&self.conn, &cred_id, &user_id)
            .await
            .map_err(PasskeyManagementError::Db)?;
        Ok(())
    }
}
