import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - ImportClock

/// The injected clock every import view model measures against.
///
/// Nothing in this module calls `Date()`. Elapsed times, "started 3 days ago",
/// and the history ordering are all differences between two instants, and a test
/// that cannot pin "now" can only assert that a duration is *some* number —
/// which is not the assertion worth writing. `CapsuleMock` makes the same choice
/// for the same reason, so a view model and the world it reads agree on what
/// time it is.
public struct ImportClock: Sendable {
    private let instant: @Sendable () -> CapsuleTimestamp

    public init(instant: @escaping @Sendable () -> CapsuleTimestamp) {
        self.instant = instant
    }

    /// The current instant.
    public func now() -> CapsuleTimestamp {
        instant()
    }

    /// The wall clock.
    public static let system = ImportClock {
        CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
    }

    /// A clock stopped at one instant.
    public static func fixed(epochSeconds: Int64) -> ImportClock {
        let stopped = CapsuleTimestamp(epochSeconds: epochSeconds)
        return ImportClock { stopped }
    }
}

// MARK: - ImportPlatform

/// The platform facts the import screens branch on.
///
/// A value rather than a `#if os(macOS)` in a view body, for two reasons. A test
/// must be able to assert that the Mac-only sources are absent on iPhone, and
/// `#if` compiles that assertion out of existence on the platform where it
/// matters. And the difference is a *capability* — "can this device watch a
/// folder?" — which is the question the picker actually asks; `os(macOS)` is
/// only today's answer to it.
public struct ImportPlatform: Sendable, Equatable, Hashable {
    /// Whether the OS lets an app observe a directory for new files. macOS only
    /// today: iOS has no equivalent of a persistent security-scoped folder
    /// watch.
    public var watchesFolders: Bool
    /// Whether removable volumes mount somewhere the app can read.
    public var mountsRemovableVolumes: Bool

    public init(watchesFolders: Bool, mountsRemovableVolumes: Bool) {
        self.watchesFolders = watchesFolders
        self.mountsRemovableVolumes = mountsRemovableVolumes
    }

    /// A Mac.
    public static let desktop = ImportPlatform(watchesFolders: true, mountsRemovableVolumes: true)
    /// An iPhone or iPad. Removable media arrives through the Files provider
    /// rather than as a mounted volume, so it is offered as a folder pick.
    public static let handheld = ImportPlatform(watchesFolders: false, mountsRemovableVolumes: false)

    /// What the running device is.
    ///
    /// Keyed on the user-visible-filesystem capability rather than on the OS
    /// name: watching a folder and mounting a volume both presuppose a file
    /// system the user can point at, which is precisely what that flag reports.
    public static var current: ImportPlatform {
        PlatformEnvironment.libraryIsSandboxPrivate ? .handheld : .desktop
    }
}
