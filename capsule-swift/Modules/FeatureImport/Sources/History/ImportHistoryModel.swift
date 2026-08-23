import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - ImportHistoryModel

/// Drives the history list: past runs, what each one did, and the two things a
/// user can do about one afterwards.
///
/// Re-running produces a **plan**, not a started import. The library has moved
/// on since: what was an import last week may be a duplicate today, and a re-run
/// that skipped the confirmation screen would be the one bulk operation in the
/// app nobody consented to.
@MainActor
@Observable
public final class ImportHistoryModel {
    public private(set) var phase: ImportPhase = .loading
    public private(set) var sessions: [ImportSessionRecord] = []
    /// Which rows are open. A set rather than a single id: comparing two past
    /// runs side by side is the reason to open one at all.
    public private(set) var expanded: Set<ImportID> = []
    /// Destination names, resolved once per album rather than per row.
    public private(set) var albumNames: [AlbumID: String] = [:]

    private let importing: any ImportPort
    private let albums: any AlbumPort
    private let connectivity: ImportConnectivity
    private let clock: ImportClock
    private let limit: Int

    public init(
        importing: any ImportPort,
        albums: any AlbumPort,
        connectivity: ImportConnectivity,
        clock: ImportClock,
        limit: Int = 50
    ) {
        self.importing = importing
        self.albums = albums
        self.connectivity = connectivity
        self.clock = clock
        self.limit = limit
    }

    public convenience init(environment: ImportEnvironment, limit: Int = 50) {
        self.init(
            importing: environment.importing,
            albums: environment.albums,
            connectivity: environment.connectivity,
            clock: environment.clock,
            limit: limit
        )
    }

    public func load() async {
        phase = .loading
        do {
            sessions = try await importing.history(limit: limit)
            await resolveAlbumNames()
            phase = sessions.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// One album lookup per distinct destination, not one per row: six rows
    /// pointing at the same album is one question, not six.
    private func resolveAlbumNames() async {
        var names: [AlbumID: String] = [:]
        for albumID in Set(sessions.map(\.destinationAlbumID)) {
            guard let album = try? await albums.containerAlbum(albumID), let name = album.name else { continue }
            names[albumID] = name
        }
        albumNames = names
    }

    /// Open or close one row.
    public func toggle(_ identifier: ImportID) {
        if expanded.contains(identifier) {
            expanded.remove(identifier)
        } else {
            expanded.insert(identifier)
        }
    }

    public func isExpanded(_ identifier: ImportID) -> Bool {
        expanded.contains(identifier)
    }

    /// Forget a run's record. The assets it brought in are untouched, which is
    /// why this is not a destructive confirmation.
    public func dismiss(_ identifier: ImportID) async {
        do {
            try await importing.dismissSession(identifier)
            sessions.removeAll { $0.id == identifier }
            expanded.remove(identifier)
            phase = sessions.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Build a fresh, confirmable plan from a past run.
    public func rerun(_ identifier: ImportID) async -> ImportPlan? {
        do {
            return try await importing.replan(identifier)
        } catch {
            phase = await connectivity.phase(for: error)
            return nil
        }
    }

    /// The destination's name, or `nil` for the nameless default album.
    public func albumName(_ identifier: AlbumID) -> String? {
        albumNames[identifier]
    }

    /// How long ago a run started, measured against the injected clock rather
    /// than `Date()`.
    public func elapsedSinceStart(of session: ImportSessionRecord) -> String {
        ImportFormat.elapsed(from: session.startedAt, to: clock.now())
    }
}

// MARK: - Presentation

public extension ImportSessionRecord.Outcome {
    var titleKey: String {
        switch self {
        case .running: "app.import.history.outcome.running"
        case .completed: "app.import.history.outcome.completed"
        case .completedWithFailures: "app.import.history.outcome.completed_with_failures"
        case .cancelled: "app.import.history.outcome.cancelled"
        }
    }

    var tone: ImportTone {
        switch self {
        case .running: .neutral
        case .completed: .positive
        case .completedWithFailures: .caution
        case .cancelled: .caution
        }
    }
}
