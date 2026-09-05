//! UniFFI bindings exposing the `capsule-core` SQLite catalog and CBOR sidecar
//! to Swift (and, in future, other UniFFI targets such as Android/Kotlin).
//!
//! One of the uniffi surfaces in the workspace — `capsule-core`'s `ffi` feature
//! exports the crypto `FfiWorkspace` + `HardwareSigner` foreign trait separately. The two
//! are **layered** on a single uniffi version (slice `S-F1`): distinct crates and distinct
//! bindings namespaces (`capsule_core_ffi` here, `capsule_core` there), never linked into the
//! same binary, so their generated scaffolding cannot collide.
//!
//! This crate is also the **app umbrella staticlib** (slice `S-F3`): it links
//! `capsule-sdk`'s uniffi surface (the `capsule_sdk` namespace — the S-D9 user-flow
//! primitives) so the iOS app consumes ONE Rust library carrying both namespaces.
//! Two separate Rust staticlibs cannot share a binary (each bundles its own std), so
//! any namespace the app needs must ride in this one. `capsule-sdk` does *not* enable
//! `capsule-core/ffi`, keeping the S-F1 never-same-binary invariant intact.
//!
//! Everything platform-specific (filesystem layout, file I/O, PhotoKit,
//! hashing) lives in the Swift client. The types defined here form the explicit
//! Rust ↔ Swift contract:
//!
//! - [`Catalog`] — a thread-safe handle over the SQLite catalog.
//! - [`render_lqip`] / [`LqipPlaceholder`] — the sidecar `lqip` record rendered to packed RGBA8
//!   through `capsule_core::lqip`, the same implementation the import pipeline encodes with and
//!   `capsule-wasm` decodes with (slice `S-B14`).
//! - [`AssetRecord`], [`AssetStackRecord`], [`StackMemberRecord`],
//!   [`AlbumRecord`] — catalog row mirrors.
//! - [`AssetSidecarRecord`] / [`serialize_sidecar`] / [`deserialize_sidecar`] —
//!   the canonical CBOR sidecar format, with unknown fields preserved verbatim.
//! - [`GatedView`] / [`LocalAuthGate`] / [`LocalAuthError`] — the fresh-local-auth seam
//!   the platform implements, and the views it unlocks ([Local Gallery — SR1]).
//! - [`CatalogError`] — the single error type crossing the boundary.
//!
//! [Local Gallery — SR1]: https://docs/design/local-gallery/#security-requirements

uniffi::setup_scaffolding!();

// Force linkage of the SDK's uniffi surface into this staticlib: nothing in this
// crate calls capsule-sdk directly (the generated Swift does, over the C ABI), and
// rustc only bundles crates it sees referenced — without this anchor the `capsule_sdk`
// scaffolding and metadata would be dropped from the archive.
use capsule_sdk as _;

mod catalog;
mod error;
mod gate;
mod records;
mod sidecar;

pub use catalog::{Catalog, LqipPlaceholder, render_lqip};
pub use error::CatalogError;
pub use gate::{GatedView, LocalAuthError, LocalAuthGate};
pub use records::{AlbumRecord, AssetRecord, AssetStackRecord, StackMemberRecord};
pub use sidecar::{AssetSidecarRecord, StackHintRecord, deserialize_sidecar, serialize_sidecar};

/// Initialise structured logging for the Rust core.
///
/// On Apple platforms this installs a `tracing` subscriber that forwards
/// `capsule-core`'s structured events into the unified logging system, where
/// they are queryable via Console.app or the `log` CLI. Safe to call more than
/// once — a second call finds a global subscriber already set and is ignored.
#[uniffi::export]
pub fn init_logging() {
    #[cfg(target_vendor = "apple")]
    {
        use tracing_subscriber::prelude::*;

        let _ = tracing_subscriber::registry()
            .with(apple_oslog::OsLogLayer::new("com.justin13888.capsule.core"))
            .try_init();
    }
}

/// The `tracing` → Apple unified-logging (`os_log`) bridge.
///
/// Replaces the former `log`-facade `oslog::OsLogger` sink: the Rust core now
/// emits `tracing` throughout (slice S-F6), so the platform bridge is a
/// [`tracing_subscriber::Layer`] driving the low-level `oslog::OsLog` bindings
/// directly rather than a `log::Log` implementation.
#[cfg(target_vendor = "apple")]
mod apple_oslog {
    use std::fmt::{self, Write as _};

    use oslog::{Level, OsLog};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level as TracingLevel, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    /// A subscriber layer that writes each event to `os_log`, one message per event.
    pub(crate) struct OsLogLayer {
        log: OsLog,
    }

    impl OsLogLayer {
        pub(crate) fn new(subsystem: &str) -> Self {
            Self {
                log: OsLog::new(subsystem, "default"),
            }
        }
    }

    /// Renders an event's message plus its structured fields into a single line.
    struct MessageVisitor {
        message: String,
    }

    impl Visit for MessageVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.message, "{value:?}");
            } else {
                let sep = if self.message.is_empty() { "" } else { " " };
                let _ = write!(self.message, "{sep}{}={value:?}", field.name());
            }
        }
    }

    impl<S: Subscriber> Layer<S> for OsLogLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MessageVisitor {
                message: String::new(),
            };
            event.record(&mut visitor);
            // Mirror oslog's own log-crate mapping (trace→Debug … error→Fault).
            let level = match *event.metadata().level() {
                TracingLevel::TRACE => Level::Debug,
                TracingLevel::DEBUG => Level::Info,
                TracingLevel::INFO => Level::Default,
                TracingLevel::WARN => Level::Error,
                TracingLevel::ERROR => Level::Fault,
            };
            self.log.with_level(level, &visitor.message);
        }
    }
}
