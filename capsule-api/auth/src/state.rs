use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::AuthConfig;
use crate::service::{AuthService, PasskeyService, PasswordService, TotpService};
use crate::session::SessionManager;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub conn: DatabaseConnection,
    pub config: AuthConfig,
    pub session_manager: SessionManager,
    pub auth_service: AuthService,
    pub password_service: PasswordService,
    pub totp_service: TotpService,
    pub passkey_service: PasskeyService,
}

impl AppState {
    pub fn new(
        conn: DatabaseConnection,
        config: AuthConfig,
        session_manager: SessionManager,
        passkey_service: PasskeyService,
    ) -> Self {
        let auth_service = AuthService::new(conn.clone(), config.clone());
        let password_service = PasswordService::new(1000); // 1s minimum
        let totp_service = TotpService::new(conn.clone(), &config.totp_issuer);

        Self {
            inner: Arc::new(AppStateInner {
                conn,
                config,
                session_manager,
                auth_service,
                password_service,
                totp_service,
                passkey_service,
            }),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
