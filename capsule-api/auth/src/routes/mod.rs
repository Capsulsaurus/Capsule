mod auth;
mod devices;
mod directory;
mod escrow;
mod passkey;
mod password;
mod profile;
mod revoke;
mod totp;

use salvo::affix_state;
use salvo::prelude::*;

use crate::state::AppState;

pub(super) fn get_router(state: AppState) -> Router {
    // The route *shape* (and thus the OpenAPI schema) is single-sourced in
    // [`route_tree`]; serving only adds the depot state injector on top. See
    // [`crate::openapi_router`] (slice `S-D8`), which reuses `route_tree` verbatim so
    // the schema dump can never drift from the routes the server actually mounts.
    route_tree().hoop(affix_state::inject(state))
}

/// The auth route tree with no injected state — the single source of truth for both the
/// live router ([`get_router`]) and the deterministic OpenAPI schema dump
/// ([`crate::openapi_router`], slice `S-D8`). State is a serving concern (depot
/// injection); the salvo `#[endpoint]` metadata that becomes the schema is carried by the
/// handler functions this tree references, so the schema is identical with or without it.
pub(super) fn route_tree() -> Router {
    Router::new()
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
        .push(
            Router::with_path("devices")
                .get(auth::get_devices)
                // Device-enrollment ceremony (skeleton — slice S-C7 in SLICES.md);
                // distinct from the session listing above.
                .push(
                    Router::with_path("enroll")
                        .post(devices::issue_enrollment_code)
                        .push(Router::with_path("redeem").post(devices::redeem_enrollment_code))
                        // Opaque relay channel for the ceremony messages: POST relays a
                        // payload into a mailbox, GET drains one. Authorized by possession of
                        // the opaque channel handle (device B is unauthenticated).
                        .push(
                            Router::with_path("channel/{channel_id}")
                                .post(devices::relay_send)
                                .get(devices::relay_recv),
                        ),
                )
                // Signed device-directory publish/fetch (slice S-C9); publish is the
                // caller's own directory, fetch is by target user id.
                .push(
                    Router::with_path("directory")
                        .post(directory::publish_device_directory)
                        .push(
                            Router::with_path("{user_id}").get(directory::fetch_device_directory),
                        ),
                ),
        )
        // Master-key escrow store/fetch/replace (slice S-C12); strictly owner-scoped —
        // store-or-replace and fetch both act on the caller's own escrow (single active
        // escrow: a store overwrites any prior blob in the same transaction).
        .push(
            Router::with_path("backup").push(
                Router::with_path("escrow")
                    .put(escrow::store_backup_escrow)
                    .get(escrow::fetch_backup_escrow),
            ),
        )
        // Single-session logout (any active session token), and beside it the *global*
        // revoke (slice S-C23), which is authenticated by an identity-key signature over a
        // server-issued challenge rather than by a session token — so a stolen token can
        // revoke only its own session, never every device.
        .push(
            Router::with_path("logout").post(auth::logout).push(
                Router::with_path("all")
                    .post(revoke::revoke_all_sessions)
                    .push(Router::with_path("challenge").post(revoke::revoke_all_challenge)),
            ),
        )
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
