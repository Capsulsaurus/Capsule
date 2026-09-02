import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - ImportEnvironment

/// The four ports the import pipeline needs, in one value.
///
/// Each *view model* still takes only the ports it actually uses — this struct
/// exists at the composition seam, not below it, so a test can build the plan
/// model with two stubs and never mention the other two.
///
/// `Sendable` because every port is; holding it is free.
public struct ImportEnvironment: Sendable {
    /// Scan, plan, execute, retry, history.
    public let importing: any ImportPort
    /// Free disk, for the space meter on the confirmation screen.
    public let storage: any StoragePort
    /// Resolves the destination album's *name*. A plan carries the album id and
    /// the rule that chose it; a screen that showed the id would be technically
    /// correct and useless.
    public let albums: any AlbumPort
    /// The connection class, for telling offline apart from failed.
    public let sync: any SyncPort
    public let clock: ImportClock
    public let platform: ImportPlatform

    public init(
        importing: any ImportPort,
        storage: any StoragePort,
        albums: any AlbumPort,
        sync: any SyncPort,
        clock: ImportClock = .system,
        platform: ImportPlatform = .current
    ) {
        self.importing = importing
        self.storage = storage
        self.albums = albums
        self.sync = sync
        self.clock = clock
        self.platform = platform
    }

    /// The connectivity probe every screen shares.
    public var connectivity: ImportConnectivity {
        ImportConnectivity(sync: sync)
    }
}
