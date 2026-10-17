import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - ScopeResolutionRow

/// One import source, with the rule that decides where its photos land.
public struct ScopeResolutionRow: Sendable, Equatable, Identifiable {
    public let scope: ImportScope
    /// The rule that fires for this scope right now.
    public let rule: ImportPlan.DestinationRule
    /// The album that rule resolves to. Absent only before the library has
    /// produced a default album at all — a state a fresh install passes
    /// through, and one the row renders rather than hides.
    public let albumID: AlbumID?

    public var id: String { scope.scopeID }

    public init(scope: ImportScope, rule: ImportPlan.DestinationRule, albumID: AlbumID?) {
        self.scope = scope
        self.rule = rule
        self.albumID = albumID
    }
}

// MARK: - ImportAndScopesSettingsModel

/// Drives the Import screen: the default album, the scope-override table, the
/// per-source-kind defaults, and the resolution order that ties them together.
///
/// The screen's job is to make a destination *explainable*. A user who imports
/// a folder and finds the photos somewhere unexpected is not helped by a
/// correct answer they cannot see the derivation of, which is why every row
/// carries the rule that produced it and the ladder is drawn in full.
@MainActor
@Observable
public final class ImportAndScopesSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var scopes: [ImportScope] = []
    public private(set) var overrides: [ImportScope: AlbumID] = [:]
    public private(set) var albums: [ContainerAlbum] = []
    public private(set) var ownerDefaultAlbumID: AlbumID?
    /// The de facto album resolution falls back to. Never absent once loaded.
    public private(set) var derivedDefaultAlbumID: AlbumID?

    /// Per-source-kind default rows.
    ///
    /// Empty in this build, and that is the documented v1 position rather than
    /// an oversight: the rows live in the E2E-encrypted library-settings
    /// document, whose write path is deferred, so *Asset Organization* says
    /// "v1 ships the base resolution — explicit user pick → the owner's
    /// `default_album_id` pointer → the derived de facto album". Held as state
    /// anyway so the rung is exercisable, and the screen says why it never
    /// fires instead of quietly omitting it.
    public private(set) var sourceKindDefaults: [SourceKind: AlbumID]

    private let settings: any SettingsPort
    private let importing: any ImportPort
    private let albumPort: any AlbumPort
    private let connectivity: SettingsConnectivity

    public init(
        settings: any SettingsPort,
        importing: any ImportPort,
        albums albumPort: any AlbumPort,
        connectivity: SettingsConnectivity,
        sourceKindDefaults: [SourceKind: AlbumID] = [:]
    ) {
        self.settings = settings
        self.importing = importing
        self.albumPort = albumPort
        self.connectivity = connectivity
        self.sourceKindDefaults = sourceKindDefaults
    }

    public func load() async {
        phase = .loading
        do {
            albums = try await albumPort.containerAlbums()
            ownerDefaultAlbumID = try await settings.defaultAlbumID()
            overrides = try await settings.scopeOverrides()
            scopes = try await importing.availableScopes()
            derivedDefaultAlbumID = albums.first(where: \.isDefault)?.id ?? albums.first?.id
            phase = scopes.isEmpty && albums.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Which rule fires for a scope, in the documented precedence.
    ///
    /// `explicitPick` is always absent here: an explicit pick is made *during*
    /// an import, and a settings screen has no import in flight. The rung is
    /// still drawn, because a user needs to know it outranks everything they
    /// can configure on this screen.
    public func rule(for scope: ImportScope) -> ImportPlan.DestinationRule {
        DestinationResolution.rule(
            explicitPick: nil,
            scopeOverride: overrides[scope],
            sourceKindDefault: sourceKindDefaults[scope.sourceKind],
            ownerPointer: ownerDefaultAlbumID
        )
    }

    /// The full table, one row per visible source.
    public var resolutions: [ScopeResolutionRow] {
        scopes.map { scope in
            let resolved = DestinationResolution.destination(
                explicitPick: nil,
                scopeOverride: overrides[scope],
                sourceKindDefault: sourceKindDefaults[scope.sourceKind],
                ownerPointer: ownerDefaultAlbumID,
                derivedDefault: derivedDefaultAlbumID
            )
            return ScopeResolutionRow(scope: scope, rule: resolved.rule, albumID: resolved.album)
        }
    }

    /// An album's display name, or the key for the nameless default album.
    ///
    /// The default album has no name by design — "a de facto, nameless
    /// container" — so this returns `nil` for it and the view renders the
    /// catalog's word for it rather than an empty row.
    public func albumName(_ id: AlbumID) -> String? {
        albums.first { $0.id == id }?.name
    }

    /// Record where one source's photos should go.
    public func setOverride(_ albumID: AlbumID?, for scope: ImportScope) async {
        do {
            try await settings.setScopeOverride(albumID, for: scope)
            overrides = try await settings.scopeOverrides()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Re-point the owner's default album.
    public func setOwnerDefault(_ albumID: AlbumID) async {
        do {
            try await settings.setDefaultAlbumID(albumID)
            ownerDefaultAlbumID = try await settings.defaultAlbumID()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}
