import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - SettingsPort

/// The local, per-device preferences.
///
/// Deliberately separate from the per-owner **library-settings document** —
/// smart-album definitions, scope overrides that sync, aggregated-album covers —
/// which is E2E-encrypted and reached through ``SmartAlbumPort`` and
/// ``FederationPort``. Conflating "settings on this phone" with "settings shared
/// across my account" is how a device-local cache budget ends up syncing to a
/// Mac with a different disk.
extension MockSystemStore: SettingsPort {
    public func settings() async throws -> LibrarySettings {
        currentSettings
    }

    public func update(_ settings: LibrarySettings) async throws {
        try settings.syncScope.requireWritable()
        try settings.uploadPolicy.requireWritable()
        setSettings(settings)
        await settingsChanges.send(settings)
    }

    /// The default album an unfiled import lands in — the **owner pointer**, not
    /// a derived value. The distinction matters because the pointer is what a
    /// user repoints before deleting the album it names.
    public func defaultAlbumID() async throws -> AlbumID? {
        currentDefaultAlbum
    }

    public func setDefaultAlbumID(_ identifier: AlbumID) async throws {
        setDefaultAlbum(identifier)
        await settingsChanges.send(currentSettings)
    }

    public func scopeOverrides() async throws -> [ImportScope: AlbumID] {
        currentOverrides
    }

    /// Record a scope's destination.
    ///
    /// Written when the user answers "where should photos from *X* go?".
    /// Automated imports never invent destinations, so an unmapped source asks
    /// exactly once — and the row is what makes a surprising destination
    /// explainable afterwards rather than merely reproducible.
    public func setScopeOverride(_ albumID: AlbumID?, for scope: ImportScope) async throws {
        setOverride(albumID, for: scope)
        await settingsChanges.send(currentSettings)
    }

    public nonisolated func changes() -> AsyncStream<LibrarySettings> {
        settingsChanges.subscribe()
    }
}
