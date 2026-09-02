import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import SwiftUI

// MARK: - SettingsEnvironment

/// Every port the settings tree needs, in one value.
///
/// The eighteen screens between them touch eleven ports, and threading eleven
/// arguments through a root view that renders none of them itself would be
/// noise. Each *view model* still takes only the ports it actually uses — this
/// struct exists at the composition seam, not below it, so a test can build a
/// view model with two stubs and never mention the other nine.
///
/// `Sendable` because every port is; holding it is free.
public struct SettingsEnvironment: Sendable {
    public let auth: any AuthPort
    public let devices: any DevicePort
    public let enrollment: any EnrollmentPort
    public let recovery: any RecoveryPort
    public let settings: any SettingsPort
    public let maintenance: any MaintenancePort
    public let sync: any SyncPort
    public let storage: any StoragePort
    public let quota: any QuotaPort
    public let uploads: any UploadPort
    public let importing: any ImportPort
    public let albums: any AlbumPort
    public let intelligence: any AIPort
    public let moderation: any ModerationPort
    public let federation: any FederationPort
    public let peering: any PeeringPort
    public let buildInfo: SettingsBuildInfo
    /// The mock world this build is running against, when it is running against
    /// one, as the raw scenario name.
    ///
    /// `nil` in a build wired to a real server — which is the honest default,
    /// and is why the Advanced screen hides its scenario switcher rather than
    /// showing an empty one. Carried as a string so this type stays free of a
    /// dependency on the mock module.
    public let activeScenarioName: String?

    public init(
        auth: any AuthPort,
        devices: any DevicePort,
        enrollment: any EnrollmentPort,
        recovery: any RecoveryPort,
        settings: any SettingsPort,
        maintenance: any MaintenancePort,
        sync: any SyncPort,
        storage: any StoragePort,
        quota: any QuotaPort,
        uploads: any UploadPort,
        importing: any ImportPort,
        albums: any AlbumPort,
        intelligence: any AIPort,
        moderation: any ModerationPort,
        federation: any FederationPort,
        peering: any PeeringPort,
        buildInfo: SettingsBuildInfo = .current(),
        activeScenarioName: String? = nil
    ) {
        self.auth = auth
        self.devices = devices
        self.enrollment = enrollment
        self.recovery = recovery
        self.settings = settings
        self.maintenance = maintenance
        self.sync = sync
        self.storage = storage
        self.quota = quota
        self.uploads = uploads
        self.importing = importing
        self.albums = albums
        self.intelligence = intelligence
        self.moderation = moderation
        self.federation = federation
        self.peering = peering
        self.buildInfo = buildInfo
        self.activeScenarioName = activeScenarioName
    }

    /// The connectivity probe every screen shares.
    public var connectivity: SettingsConnectivity {
        SettingsConnectivity(sync: sync)
    }
}

// MARK: - SettingsBuildInfo

/// The build facts the About and Advanced screens display.
///
/// Injected rather than read from `Bundle` inside a view model, because two of
/// these five values are load-bearing in a support conversation — the
/// `client_version` string is written into every manifest this device signs,
/// and the `protocol_version` decides which albums it may write to at all — so
/// a test must be able to pin them.
///
/// ``protocolVersion`` and ``cryptoSuiteID`` have no port of their own yet; the
/// values live in `capsule-core` and reach the client through manifests. They
/// are injected here so the screen is honest today and correct the moment a
/// port surfaces them.
public struct SettingsBuildInfo: Sendable, Equatable, Hashable {
    /// `CFBundleShortVersionString`.
    public var marketingVersion: String
    /// `CFBundleVersion`.
    public var buildNumber: String
    /// The exact string written into a manifest's `client_version` field, and
    /// therefore the string a provenance record will show for anything this
    /// device signs.
    public var clientVersion: String
    /// The date-based protocol version this build speaks.
    public var protocolVersion: String
    /// The primitive bundle identifier this build writes under.
    public var cryptoSuiteID: UInt16
    /// The platform tag the device-cohort hash is domain-separated by.
    public var platformTag: String
    /// The OS name and version, as a support report would quote them.
    public var systemDescription: String
    /// The hardware model identifier.
    public var hardwareModel: String

    public init(
        marketingVersion: String,
        buildNumber: String,
        clientVersion: String,
        protocolVersion: String,
        cryptoSuiteID: UInt16,
        platformTag: String,
        systemDescription: String,
        hardwareModel: String
    ) {
        self.marketingVersion = marketingVersion
        self.buildNumber = buildNumber
        self.clientVersion = clientVersion
        self.protocolVersion = protocolVersion
        self.cryptoSuiteID = cryptoSuiteID
        self.platformTag = platformTag
        self.systemDescription = systemDescription
        self.hardwareModel = hardwareModel
    }

    /// The protocol version this build writes under when nothing overrides it.
    public static let defaultProtocolVersion = "2026-05-01"
    /// The primitive bundle this build writes under when nothing overrides it.
    public static let defaultCryptoSuiteID: UInt16 = 1

    /// Read what the bundle knows, and compose the `client_version` string the
    /// same way for every manifest so a provenance chain reads consistently.
    public static func current(bundle: Bundle = .main) -> SettingsBuildInfo {
        let info = bundle.infoDictionary
        let marketing = info?["CFBundleShortVersionString"] as? String ?? SettingsFormat.unknown
        let build = info?["CFBundleVersion"] as? String ?? SettingsFormat.unknown
        return SettingsBuildInfo(
            marketingVersion: marketing,
            buildNumber: build,
            clientVersion: "capsule-\(PlatformEnvironment.platformTag)/\(marketing)+\(build)",
            protocolVersion: defaultProtocolVersion,
            cryptoSuiteID: defaultCryptoSuiteID,
            platformTag: PlatformEnvironment.platformTag,
            systemDescription: "\(PlatformEnvironment.systemName) \(PlatformEnvironment.systemVersion)",
            hardwareModel: PlatformEnvironment.hardwareModel
        )
    }
}

// MARK: - SettingsLinks

/// Destinations the settings tree links to but does not own.
///
/// The Diagnostics report screen lives in the app target, above this module, so
/// this module cannot name its type. Rather than duplicating it — two consent
/// screens is one consent screen too many — the Diagnostics section links out
/// through this hook, and renders an explanation with no link when the host
/// supplies none.
@MainActor
public struct SettingsLinks {
    /// The app's existing diagnostics consent and report screen.
    public var diagnosticsReport: (() -> AnyView)?

    public init(diagnosticsReport: (() -> AnyView)? = nil) {
        self.diagnosticsReport = diagnosticsReport
    }
}
