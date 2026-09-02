//! Localized CLI output over `capsule-i18n`.
//!
//! Every user-facing string the CLI prints is a key in the canonical `locales/`
//! catalogs (namespace `cli.*`), rendered through a [`Bundle`] negotiated from the
//! process locale. This module centralizes the bundle and the key constants so a typo
//! is a compile error, not a silently-missing message.
//!
//! "User-facing" here means what `xtask i18n-guard` enforces for this crate: terminal
//! output (`println!`/`eprintln!`) and the errors `color_eyre` prints. `tracing::*!` is
//! operator telemetry and stays English. Commands migrated so far: the networked ones
//! (`auth`, `sync`, `list`, `push` — `S-D5`), `cull` (`S-D16`), and `import` (`S-I5`);
//! the rest are carried as visible debt in `locales/i18n-guard-allowlist.txt`.
//!
//! `--help` is user-facing too, and is rendered from the [`HELP_NAMESPACE`] keys by
//! [`crate::cli::help`] (`S-I8`) rather than from a key constant per string: its keys are
//! derived from the command tree, so a new subcommand's help is a catalog entry the
//! invariant test in that module demands, not a constant somebody has to remember to add.

pub use capsule_i18n::{Bundle, Value, error_codes};
use capsule_i18n::{negotiate, supported_locales};

/// The namespace `--help` text lives under; the key grammar is [`crate::cli::help`]'s.
pub const HELP_NAMESPACE: &str = "cli.help";

/// Build the CLI's message bundle from the POSIX locale environment, falling back
/// to the source locale. `LC_ALL` wins, then `LC_MESSAGES`, then `LANG`.
pub fn cli_bundle() -> Bundle {
    let requested = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    // POSIX locales look like `en_US.UTF-8`; reduce to a BCP-47-ish tag.
    let tag = requested
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let locale = negotiate(&tag, supported_locales(), "en");
    Bundle::for_locale(&locale)
}

/// Catalog keys for the CLI's networked commands (slice `S-D5`).
pub mod keys {
    pub const AUTH_LOGIN_EMAIL_PROMPT: &str = "cli.auth.login.email_prompt";
    pub const AUTH_LOGIN_PASSWORD_PROMPT: &str = "cli.auth.login.password_prompt";
    pub const AUTH_LOGIN_TOTP_PROMPT: &str = "cli.auth.login.totp_prompt";
    pub const AUTH_LOGIN_SECOND_FACTOR_REQUIRED: &str = "cli.auth.login.second_factor_required";
    pub const AUTH_LOGIN_IN_PROGRESS: &str = "cli.auth.login.in_progress";
    pub const AUTH_LOGIN_SUCCESS: &str = "cli.auth.login.success";
    pub const AUTH_LOGIN_FAILED: &str = "cli.auth.login.failed";
    pub const AUTH_REGISTER_IN_PROGRESS: &str = "cli.auth.register.in_progress";
    pub const AUTH_REGISTER_SUCCESS: &str = "cli.auth.register.success";
    pub const AUTH_REGISTER_FAILED: &str = "cli.auth.register.failed";
    pub const AUTH_LOGOUT_IN_PROGRESS: &str = "cli.auth.logout.in_progress";
    pub const AUTH_LOGOUT_SUCCESS: &str = "cli.auth.logout.success";
    pub const AUTH_LOGOUT_NOT_LOGGED_IN: &str = "cli.auth.logout.not_logged_in";
    pub const AUTH_NOT_AUTHENTICATED: &str = "cli.auth.not_authenticated";
    pub const SYNC_IN_PROGRESS: &str = "cli.sync.in_progress";
    pub const SYNC_DRY_RUN_NOTICE: &str = "cli.sync.dry_run_notice";
    pub const SYNC_UP_TO_DATE: &str = "cli.sync.up_to_date";
    pub const SYNC_COMPLETE: &str = "cli.sync.complete";
    pub const SYNC_FAILED: &str = "cli.sync.failed";
    pub const LIST_EMPTY: &str = "cli.list.empty";
    pub const LIST_HEADER: &str = "cli.list.header";
    pub const LIST_ROW: &str = "cli.list.row";
    pub const LIST_STATE_ACTIVE: &str = "cli.list.state.active";
    pub const LIST_STATE_AWAITING_ORIGINAL: &str = "cli.list.state.awaiting_original";
    pub const LIST_FAILED: &str = "cli.list.failed";
    pub const PUSH_IN_PROGRESS: &str = "cli.push.in_progress";
    pub const PUSH_DRY_RUN_NOTICE: &str = "cli.push.dry_run_notice";
    pub const PUSH_FORCE_NOTICE: &str = "cli.push.force_notice";
    pub const PUSH_NOTHING_TO_PUSH: &str = "cli.push.nothing_to_push";
    pub const PUSH_UP_TO_DATE: &str = "cli.push.up_to_date";
    pub const PUSH_COMPLETE: &str = "cli.push.complete";
    pub const PUSH_DRY_RUN_COMPLETE: &str = "cli.push.dry_run_complete";
    pub const PUSH_FAILED: &str = "cli.push.failed";
    // `capsule import` — the local import arm (slice `S-I5`). Import never touches the
    // network, but every line it prints and every error it surfaces still reaches a human.
    pub const IMPORT_IN_PROGRESS: &str = "cli.import.in_progress";
    pub const IMPORT_SCANNING: &str = "cli.import.scanning";
    pub const IMPORT_PROVIDER_NOTICE: &str = "cli.import.provider_notice";
    pub const IMPORT_CANDIDATES_FOUND: &str = "cli.import.candidates_found";
    pub const IMPORT_PLAN_SUMMARY: &str = "cli.import.plan_summary";
    pub const IMPORT_NOTHING_TO_IMPORT: &str = "cli.import.nothing_to_import";
    pub const IMPORT_EXECUTING: &str = "cli.import.executing";
    pub const IMPORT_OUTCOME_DUPLICATE: &str = "cli.import.outcome.duplicate";
    pub const IMPORT_OUTCOME_CORRUPT_TRANSFER: &str = "cli.import.outcome.corrupt_transfer";
    pub const IMPORT_OUTCOME_UNREADABLE: &str = "cli.import.outcome.unreadable";
    pub const IMPORT_DONE: &str = "cli.import.done";
    pub const IMPORT_SCAN_FAILED: &str = "cli.import.scan_failed";
    pub const IMPORT_EXTRACT_FAILED: &str = "cli.import.extract_failed";
    pub const IMPORT_PLAN_FAILED: &str = "cli.import.plan_failed";
    pub const IMPORT_EXECUTE_FAILED: &str = "cli.import.execute_failed";
    // `capsule cull` — the standalone culling command (slice `S-D16`).
    pub const CULL_FLAGGED: &str = "cli.cull.flagged";
    pub const CULL_VIEW: &str = "cli.cull.view";
    pub const CULL_FILTER_HEADER: &str = "cli.cull.filter_header";
    pub const CULL_FILTER_EMPTY: &str = "cli.cull.filter_empty";
    pub const CULL_ROW: &str = "cli.cull.row";
    pub const CULL_SWEPT: &str = "cli.cull.swept";
    pub const CULL_NOTHING_TO_SWEEP: &str = "cli.cull.nothing_to_sweep";
    pub const CULL_UNKNOWN_ASSET: &str = "cli.cull.unknown_asset";
    pub const CULL_FAILED: &str = "cli.cull.failed";
    pub const CULL_FLAG_PICK: &str = "cli.cull.flag.pick";
    pub const CULL_FLAG_NEUTRAL: &str = "cli.cull.flag.neutral";
    pub const CULL_FLAG_REJECT: &str = "cli.cull.flag.reject";
    // `capsule show` — the signed-sidecar read surface (slice `S-B18`). One row key per
    // field, each taking the rendered `{value}`; the `value.*` keys spell the composite and
    // absent values that go into a row.
    pub const SHOW_HEADER: &str = "cli.show.header";
    pub const SHOW_ALBUM: &str = "cli.show.album";
    pub const SHOW_CONTENT_TYPE: &str = "cli.show.content_type";
    pub const SHOW_HASH: &str = "cli.show.hash";
    pub const SHOW_DIMENSIONS: &str = "cli.show.dimensions";
    pub const SHOW_CAPTURED: &str = "cli.show.captured";
    pub const SHOW_IMPORTED: &str = "cli.show.imported";
    pub const SHOW_CAPTION: &str = "cli.show.caption";
    pub const SHOW_RATING: &str = "cli.show.rating";
    pub const SHOW_TAGS_USER: &str = "cli.show.tags_user";
    pub const SHOW_TAGS_AI: &str = "cli.show.tags_ai";
    pub const SHOW_GPS: &str = "cli.show.gps";
    pub const SHOW_CULL: &str = "cli.show.cull";
    pub const SHOW_HIDDEN: &str = "cli.show.hidden";
    pub const SHOW_STACK: &str = "cli.show.stack";
    pub const SHOW_LQIP: &str = "cli.show.lqip";
    pub const SHOW_PROVENANCE_RECORDS: &str = "cli.show.provenance_records";
    pub const SHOW_VALUE_UNSET: &str = "cli.show.value.unset";
    pub const SHOW_VALUE_YES: &str = "cli.show.value.yes";
    pub const SHOW_VALUE_NO: &str = "cli.show.value.no";
    pub const SHOW_VALUE_PRESENT: &str = "cli.show.value.present";
    pub const SHOW_VALUE_LIST_SEPARATOR: &str = "cli.show.value.list_separator";
    pub const SHOW_GPS_DATUM_WGS84: &str = "cli.show.gps_datum.wgs84";
    pub const SHOW_GPS_DATUM_GCJ02: &str = "cli.show.gps_datum.gcj02";
    pub const SHOW_VALUE_DIMENSIONS: &str = "cli.show.value.dimensions";
    pub const SHOW_VALUE_RATING: &str = "cli.show.value.rating";
    pub const SHOW_VALUE_GPS: &str = "cli.show.value.gps";
    pub const SHOW_VALUE_STACK: &str = "cli.show.value.stack";
    pub const SHOW_GPS_SOURCE_EXIF: &str = "cli.show.gps_source.exif";
    pub const SHOW_GPS_SOURCE_MANUAL: &str = "cli.show.gps_source.manual";
    pub const SHOW_GPS_SOURCE_DERIVED: &str = "cli.show.gps_source.derived";
    pub const SHOW_STACK_ROLE_PRIMARY: &str = "cli.show.stack_role.primary";
    pub const SHOW_STACK_ROLE_MEMBER: &str = "cli.show.stack_role.member";
    pub const SHOW_STACK_ROLE_PROXY: &str = "cli.show.stack_role.proxy";
    pub const SHOW_UNKNOWN_ASSET: &str = "cli.show.unknown_asset";
    pub const SHOW_AMBIGUOUS: &str = "cli.show.ambiguous";
    pub const SHOW_INVALID_SELECTOR: &str = "cli.show.invalid_selector";
    pub const SHOW_FAILED: &str = "cli.show.failed";
    // `capsule repair capture-time` — the capture-timestamp repair pass (slice `S-B17`).
    pub const REPAIR_CAPTURE_TIME_CHECKED: &str = "cli.repair.capture_time.checked";
    pub const REPAIR_CAPTURE_TIME_DRY_RUN_NOTICE: &str = "cli.repair.capture_time.dry_run_notice";
    pub const REPAIR_CAPTURE_TIME_ROW: &str = "cli.repair.capture_time.row";
    pub const REPAIR_CAPTURE_TIME_ROW_UNPARSEABLE: &str = "cli.repair.capture_time.row_unparseable";
    pub const REPAIR_CAPTURE_TIME_UNREADABLE: &str = "cli.repair.capture_time.unreadable";
    pub const REPAIR_CAPTURE_TIME_CORRECTED: &str = "cli.repair.capture_time.corrected";
    pub const REPAIR_CAPTURE_TIME_LIMIT_NOTICE: &str = "cli.repair.capture_time.limit_notice";
    pub const REPAIR_CAPTURE_TIME_SUMMARY: &str = "cli.repair.capture_time.summary";
    pub const REPAIR_CAPTURE_TIME_NOTHING: &str = "cli.repair.capture_time.nothing";
    pub const REPAIR_CAPTURE_TIME_DRIFT_NOTICE: &str = "cli.repair.capture_time.drift_notice";
    pub const REPAIR_CAPTURE_TIME_FAILED_ASSET: &str = "cli.repair.capture_time.failed_asset";
    pub const REPAIR_FAILED: &str = "cli.repair.failed";
}
