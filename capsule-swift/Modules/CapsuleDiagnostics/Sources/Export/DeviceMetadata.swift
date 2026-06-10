import Foundation
import UIKit

/// Non-identifying device & app facts attached to a diagnostics bundle.
///
/// Deliberately excludes every stable identifier (no UDID / IDFV / IDFA) and any
/// user content — only coarse environment facts useful for triage. The free-disk
/// figure is bucketed rather than exact to avoid a fingerprintable value.
public struct DeviceMetadata: Codable, Sendable, Equatable {
    public let appVersion: String
    public let appBuild: String
    public let systemName: String
    public let systemVersion: String
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

    /// Snapshot the current device & app environment. Main-actor because it
    /// touches `UIDevice`.
    @MainActor
    public static func current(bundle: Bundle = .main) -> DeviceMetadata {
        let device = UIDevice.current
        let info = bundle.infoDictionary
        return DeviceMetadata(
            appVersion: info?["CFBundleShortVersionString"] as? String ?? "unknown",
            appBuild: info?["CFBundleVersion"] as? String ?? "unknown",
            systemName: device.systemName,
            systemVersion: device.systemVersion,
            model: device.model,
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
        case ..<500_000_000: .critical
        case ..<2_000_000_000: .low
        case ..<10_000_000_000: .moderate
        default: .ample
        }
    }
}
