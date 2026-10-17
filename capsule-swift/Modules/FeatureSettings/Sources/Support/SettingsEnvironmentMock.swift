import CapsuleMock
import CapsuleNavigation
import Foundation

// MARK: - Mock composition

public extension SettingsEnvironment {
    /// Wire the settings tree to a mock world.
    ///
    /// This is what every `#Preview` in the module uses and what the app's
    /// composition root uses until the SDK lands. Building it from a
    /// ``MockEnvironment`` rather than from loose stubs is deliberate: a
    /// scenario is coherent across ports by construction, so the Storage screen
    /// and the Sync screen cannot disagree about whether the device is offline.
    init(mock: MockEnvironment, buildInfo: SettingsBuildInfo = .preview) {
        self.init(
            auth: mock.auth,
            devices: mock.devices,
            enrollment: mock.enrollment,
            recovery: mock.recovery,
            settings: mock.settings,
            maintenance: mock.maintenance,
            sync: mock.sync,
            storage: mock.storage,
            quota: mock.quota,
            uploads: mock.uploads,
            importing: mock.importing,
            albums: mock.albums,
            intelligence: mock.intelligence,
            moderation: mock.moderation,
            federation: mock.federation,
            peering: mock.peering,
            buildInfo: buildInfo,
            activeScenarioName: mock.scenario.rawValue
        )
    }

    /// A settings environment for one named mock world.
    static func preview(_ scenario: MockScenario = .healthy) -> SettingsEnvironment {
        SettingsEnvironment(mock: MockEnvironment(scenario: scenario))
    }
}

public extension SettingsBuildInfo {
    /// Fixed build facts, so a preview snapshot does not change with the build
    /// number and a test can assert on the strings.
    static let preview = SettingsBuildInfo(
        marketingVersion: "0.1.0",
        buildNumber: "1",
        clientVersion: "capsule-ios/0.1.0+1",
        protocolVersion: SettingsBuildInfo.defaultProtocolVersion,
        cryptoSuiteID: SettingsBuildInfo.defaultCryptoSuiteID,
        platformTag: "ios",
        systemDescription: "iOS 26.0",
        hardwareModel: "iPhone17,1"
    )
}

// MARK: - MockScenarioSelection

/// The persisted mock-scenario override the Advanced screen writes.
///
/// A scenario is chosen at launch — ``MockEnvironment`` builds its whole world
/// from one, and half the app's screens are unreachable from a healthy library
/// — so switching one cannot take effect in place. The switcher therefore
/// records the choice and says plainly that it applies on next launch, rather
/// than pretending to rebuild a graph it does not own.
///
/// `@MainActor` rather than `Sendable`: it wraps `UserDefaults`, every caller is
/// a main-actor view model, and pretending otherwise would be a concurrency
/// annotation that buys nothing.
@MainActor
public struct MockScenarioSelection {
    /// The defaults key. Shared with the app's composition root, which reads it
    /// at launch and turns it into the launch argument the UI tests also use.
    public static let defaultsKey = "capsule.settings.mock_scenario"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The scenario chosen for the *next* launch, if one was chosen.
    public func selected() -> MockScenario? {
        guard let raw = defaults.string(forKey: Self.defaultsKey) else { return nil }
        return MockScenario(rawValue: raw)
    }

    /// Record a choice for the next launch.
    public func select(_ scenario: MockScenario) {
        defaults.set(scenario.rawValue, forKey: Self.defaultsKey)
    }

    /// Forget the override, so the next launch uses whatever the launch
    /// arguments say.
    public func clear() {
        defaults.removeObject(forKey: Self.defaultsKey)
    }
}
