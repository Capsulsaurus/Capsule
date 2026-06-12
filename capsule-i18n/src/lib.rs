//! Runtime localization for Capsule's Rust surfaces (server, CLI, core).
//!
//! User-facing strings are authored once in the repo-root `locales/` directory as
//! [ICU MessageFormat](https://unicode-org.github.io/icu/userguide/format_parse/messages/)
//! JSON, then compiled into this crate by `cargo run -p xtask -- i18n` (see
//! [`generated`]). The same source compiles to the web, iOS, and Android catalogs,
//! so a message is written exactly once.
//!
//! Typical use on the server: negotiate the request's locale, build a [`Bundle`],
//! and format keys (often a stable [`error_codes`] identifier):
//!
//! ```
//! use capsule_i18n::{Bundle, error_codes, negotiate, supported_locales};
//!
//! let locale = negotiate("fr-CA, en;q=0.8", supported_locales(), "en");
//! let bundle = Bundle::for_locale(&locale);
//! let message = bundle.format(error_codes::AUTH_INVALID_CREDENTIALS, &[]);
//! assert_eq!(message, "Invalid email or password.");
//! ```
//!
//! The full design (canonical format, locale resolution, the server error-code
//! contract) lives in the i18n design doc.

mod catalog;
mod format;
mod generated;
mod negotiate;

pub use catalog::{Bundle, supported_locales};
pub use format::{Value, format_message};
pub use generated::error_codes;
pub use negotiate::negotiate;
