mod auth;
mod passkey;
mod password;
mod profile;
mod totp;

use salvo::affix_state;
use salvo::prelude::*;

use crate::state::AppState;

pub(super) fn get_router(state: AppState) -> Router {
    Router::new()
        // Inject state into depot for all routes
        .hoop(affix_state::inject(state))
        // Profile routes
        .push(
            Router::with_path("profile")
                .get(profile::get_user_profile)
                .post(profile::update_user_profile),
        )
        // Auth routes
        .push(Router::with_path("register").post(auth::register_user))
        .push(
            Router::with_path("login")
                .post(auth::login_user)
                .push(Router::with_path("verify-totp").post(totp::totp_verify_login)),
        )
        .push(Router::with_path("refresh").post(auth::refresh_token))
        .push(Router::with_path("validate").post(auth::validate_token))
        .push(Router::with_path("devices").get(auth::get_devices))
        .push(Router::with_path("logout").post(auth::logout))
        // Password routes
        .push(Router::with_path("password-reset-request").post(password::reset_password_request))
        .push(Router::with_path("password-reset").post(password::reset_password))
        // TOTP routes
        .push(
            Router::with_path("totp")
                .push(Router::with_path("enroll").post(totp::totp_enroll))
                .push(Router::with_path("verify-enrollment").post(totp::totp_verify_enrollment))
                .push(Router::with_path("disable").post(totp::totp_disable)),
        )
        // Passkey routes
        .push(
            Router::with_path("passkey")
                .push(
                    Router::with_path("register")
                        .push(Router::with_path("start").post(passkey::start_registration))
                        .push(Router::with_path("finish").post(passkey::finish_registration)),
                )
                .push(
                    Router::with_path("login")
                        .push(Router::with_path("start").post(passkey::start_authentication))
                        .push(Router::with_path("finish").post(passkey::finish_authentication)),
                )
                .push(Router::with_path("credentials").get(passkey::list_credentials))
                .push(Router::with_path("credentials/:cred_id").delete(passkey::delete_credential)),
        )
}

// TODO: Alerting
// - Multiple failed login attempts
// - Unusual authentication patterns
// - Rate limit threshold breaches
