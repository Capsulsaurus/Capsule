import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - QuarantinePort

/// The inventory of things that need a human to look at.
///
/// The invariant this port exists to serve: a quarantined item is **never
/// silently dropped and never silently applied**. There is deliberately no
/// `resolveAll()` and no automatic resolution — automatic resolution *is*
/// silently applying or silently dropping, which is the behaviour the whole
/// surface exists to prevent.
public protocol QuarantinePort: Sendable {
    /// Everything currently held, newest first.
    ///
    /// Maps to `quarantine.list`.
    func items(offset: Int, limit: Int) async throws -> Page<QuarantineItem>

    /// Held items on one of the eight surfaces.
    ///
    /// Maps to `quarantine.list_by_surface`.
    func items(on surface: QuarantineSurface, offset: Int, limit: Int) async throws -> Page<QuarantineItem>

    /// How many items are held, for a badge.
    ///
    /// Maps to `quarantine.count`.
    func itemCount() async throws -> Int

    /// Read the preserved bytes without changing anything.
    ///
    /// Returns `nil` when the holding area records the event but not the bytes —
    /// an audit-log entry has nothing to inspect.
    ///
    /// Maps to `quarantine.inspect`.
    func inspect(_ id: QuarantineID) async throws -> Data?

    /// Attempt recovery: re-fetch, re-derive, re-run the ceremony, adopt.
    ///
    /// - Throws: when the item's holding area does not preserve enough state for
    ///   repair to mean anything. Check ``QuarantineItem/isRecoverable`` first.
    ///
    /// Maps to `quarantine.repair`.
    func repair(_ id: QuarantineID) async throws

    /// Discard an item and its preserved bytes.
    ///
    /// **Irreversible.** Never the default, and never bundled with another
    /// action.
    ///
    /// Maps to `quarantine.discard`.
    func discard(_ id: QuarantineID) async throws

    /// A stream that fires when something is quarantined or resolved.
    func changes() -> AsyncStream<Void>
}

// MARK: - MaintenancePort

/// Scheduled integrity and housekeeping work.
public protocol MaintenancePort: Sendable {
    /// Every job and where it stands.
    ///
    /// Maps to `maintenance.tasks`.
    func tasks() async throws -> [MaintenanceTask]

    /// Run a job now, bypassing its schedule but **not** its gating.
    ///
    /// A ``MaintenanceTaskKind/isDestructive`` job still runs behind
    /// verify-before-destroy: asking for it explicitly does not waive the gate
    /// that stops it deleting an only copy.
    ///
    /// Maps to `maintenance.run_task`.
    func run(_ kind: MaintenanceTaskKind) -> AsyncStream<MaintenanceTask>

    /// Cancel a running job. Cancellation is a stop, not a rollback: partial
    /// progress stands.
    ///
    /// Maps to `maintenance.cancel_task`.
    func cancel(_ kind: MaintenanceTaskKind) async throws

    /// A stream of task-state updates.
    func changes() -> AsyncStream<[MaintenanceTask]>
}

// MARK: - SettingsPort

/// The local, per-device preferences.
///
/// Deliberately separate from the per-owner **library-settings document** —
/// smart-album definitions, scope overrides, aggregated-album covers — which is
/// E2E-encrypted, syncs across devices as CRDTs, and is reached through
/// ``SmartAlbumPort`` and ``FederationPort``. Conflating "settings on this
/// phone" with "settings shared across my account" is how a device-local cache
/// budget ends up syncing to a Mac with a different disk.
public protocol SettingsPort: Sendable {
    /// The current settings.
    ///
    /// Maps to `settings.get_local`.
    func settings() async throws -> LibrarySettings

    /// Replace the settings wholesale.
    ///
    /// Maps to `settings.put_local`.
    func update(_ settings: LibrarySettings) async throws

    /// The default album an unfiled import lands in — the owner pointer, not a
    /// derived value.
    ///
    /// Maps to `settings.get_default_album`.
    func defaultAlbumID() async throws -> AlbumID?

    /// Re-point the default album.
    ///
    /// Maps to `settings.set_default_album`.
    func setDefaultAlbumID(_ id: AlbumID) async throws

    /// The scope → album mapping rows this device has recorded, so a
    /// destination is explainable after the fact.
    ///
    /// Maps to `settings.list_scope_overrides`.
    func scopeOverrides() async throws -> [ImportScope: AlbumID]

    /// Record a scope's destination — written when the user answers "where
    /// should photos from *X* go?". Automated imports never invent destinations,
    /// so an unmapped source asks exactly once.
    ///
    /// Maps to `settings.set_scope_override`.
    func setScopeOverride(_ albumID: AlbumID?, for scope: ImportScope) async throws

    /// A stream that fires when settings change, including from another
    /// device's sync where the row is shared.
    func changes() -> AsyncStream<LibrarySettings>
}
