import CapsuleFoundation
import Foundation

/// Non-identifying device & app facts attached to a diagnostics bundle.
///
/// Deliberately excludes every stable identifier (no UDID / IDFV / IDFA) and any
/// user content — only coarse environment facts useful for triage. The free-disk
/// figure is bucketed rather than exact to avoid a fingerprintable value.
///
/// The field set is identical on iOS and macOS so a bundle from either platform
/// decodes with the same schema and the triage tooling needs no branch.
public struct DeviceMetadata: Codable, Sendable, Equatable {
    public let appVersion: String
    public let appBuild: String
    public let systemName: String
    public let systemVersion: String
    /// The hardware model identifier — `"iPhone17,1"`, `"Mac16,7"`.
    ///
    /// A *product* identifier, not a per-unit one: every device of that model
    /// reports the same string, so it identifies the hardware to triage against
    /// without identifying the user's device. That is what keeps this struct's
    /// "no stable identifier" claim accurate.
    public let model: String
    public let locale: String
    public let freeDiskSpace: DiskSpaceBucket

    public init(
        appVersion: String,
        appBuild: String,
        systemName: String,
        systemVersion: String,
        model: String,
        locale: String,
        freeDiskSpace: DiskSpaceBucket
    ) {
        self.appVersion = appVersion
        self.appBuild = appBuild
        self.systemName = systemName
        self.systemVersion = systemVersion
        self.model = model
        self.locale = locale
        self.freeDiskSpace = freeDiskSpace
    }

    /// Snapshot the current device & app environment.
    ///
    /// The host facts come from `PlatformEnvironment`, which is the only part
    /// of the app allowed to know which OS it is running on; this module just
    /// asks. That is what lets the same code build for iOS and macOS.
    ///
    /// Stays `@MainActor` for source compatibility with the callers that
    /// already `await` it, though nothing it reads is main-actor state any more.
    @MainActor
    public static func current(bundle: Bundle = .main) -> DeviceMetadata {
        let info = bundle.infoDictionary
        return DeviceMetadata(
            appVersion: info?["CFBundleShortVersionString"] as? String ?? "unknown",
            appBuild: info?["CFBundleVersion"] as? String ?? "unknown",
            systemName: PlatformEnvironment.systemName,
            systemVersion: PlatformEnvironment.systemVersion,
            model: PlatformEnvironment.hardwareModel,
            locale: Locale.current.identifier,
            freeDiskSpace: DiskSpaceBucket.current()
        )
    }
}

/// A coarse free-disk bucket — avoids reporting an exact, fingerprintable byte count.
public enum DiskSpaceBucket: String, Codable, Sendable, Equatable, CaseIterable {
    case critical // < 500 MB
    case low // < 2 GB
    case moderate // < 10 GB
    case ample // >= 10 GB
    case unknown

    static func current() -> DiskSpaceBucket {
        guard let bytes = try? URL(fileURLWithPath: NSHomeDirectory())
            .resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
            .volumeAvailableCapacityForImportantUsage
        else { return .unknown }
        return bucket(forBytes: bytes)
    }

    static func bucket(forBytes bytes: Int64) -> DiskSpaceBucket {
        switch bytes {
        case ..<500000000: .critical
        case ..<2000000000: .low
        case ..<10000000000: .moderate
        default: .ample
        }
    }
}
