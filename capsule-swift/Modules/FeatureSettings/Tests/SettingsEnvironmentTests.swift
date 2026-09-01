import CapsuleFoundation
import CapsuleMock
import FeatureSettings
import Foundation
import Testing

// MARK: - SettingsEnvironmentTests

/// The composition seam. A scenario is coherent across ports by construction,
/// so the Storage screen and the Sync screen cannot disagree about whether the
/// device is offline — and no screen in this module ever reaches a real system
/// service to find out.
@Suite("The settings tree is wired to a mock world, never to a real service")
struct SettingsEnvironmentTests {
    @Test("a preview environment names the world it was built from")
    func previewCarriesItsScenario() {
        let healthy = SettingsEnvironment.preview(.healthy)
        let offline = SettingsEnvironment.preview(.offline)

        #expect(healthy.activeScenarioName == MockScenario.healthy.rawValue)
        #expect(offline.activeScenarioName == MockScenario.offline.rawValue)
    }

    /// A build wired to a real server has no scenario, which is why the
    /// Advanced screen hides its switcher rather than showing an empty one.
    @Test("an environment built without a mock world reports no scenario")
    func realBuildHasNoScenario() {
        let mock = MockEnvironment(scenario: .healthy)
        let environment = SettingsEnvironment(
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
            buildInfo: .preview
        )

        #expect(environment.activeScenarioName == nil)
    }

    @Test("the offline world is offline through the connectivity probe every screen shares")
    func offlineWorldIsOfflineEverywhere() async {
        let offline = SettingsEnvironment.preview(.offline)
        let healthy = SettingsEnvironment.preview(.healthy)

        let offlineClass = await offline.connectivity.connectionClass()
        let healthyClass = await healthy.connectivity.connectionClass()

        #expect(offlineClass == .offline)
        #expect(healthyClass?.isUsable == true)
    }

    @Test("the preview build facts are pinned, so a snapshot does not move with the build number")
    func previewBuildInfoIsPinned() {
        let info = SettingsBuildInfo.preview

        #expect(info.marketingVersion == "0.1.0")
        #expect(info.buildNumber == "1")
        #expect(info.clientVersion == "capsule-ios/0.1.0+1")
        #expect(info.protocolVersion == SettingsBuildInfo.defaultProtocolVersion)
        #expect(info.cryptoSuiteID == SettingsBuildInfo.defaultCryptoSuiteID)
    }

    /// The `client_version` string is written into every manifest this device
    /// signs, so its grammar is a wire contract rather than a display detail.
    @Test("client_version is composed as capsule-<platform>/<marketing>+<build>")
    func clientVersionFollowsItsGrammar() {
        let info = SettingsBuildInfo.current()

        #expect(info.clientVersion.hasPrefix("capsule-\(PlatformEnvironment.platformTag)/"))
        #expect(info.clientVersion.contains("+"))
        #expect(info.clientVersion == "capsule-\(info.platformTag)/\(info.marketingVersion)+\(info.buildNumber)")
        #expect(info.protocolVersion == SettingsBuildInfo.defaultProtocolVersion)
    }
}

// MARK: - MockScenarioSelectionTests

/// A scenario is chosen at launch, so switching one cannot take effect in
/// place. The switcher records the choice; it does not pretend to rebuild a
/// graph it does not own.
@Suite("The scenario switcher records a choice for the next launch")
@MainActor
struct MockScenarioSelectionTests {
    /// An isolated defaults suite, torn down when the test ends: a settings
    /// test must never read or write the machine's own preferences.
    private final class IsolatedDefaults {
        let name = "capsule.tests.\(UUID().uuidString)"
        let defaults: UserDefaults

        init() throws {
            guard let suite = UserDefaults(suiteName: name) else {
                throw CocoaError(.fileNoSuchFile)
            }
            defaults = suite
        }

        deinit {
            defaults.removePersistentDomain(forName: name)
        }
    }

    @Test("nothing is selected until something is selected")
    func noSelectionByDefault() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)

        #expect(selection.selected() == nil)
        #expect(MockScenarioSelection.defaultsKey == "capsule.settings.mock_scenario")
    }

    @Test("a choice round-trips through the defaults key the launch path reads")
    func choiceRoundTrips() throws {
        let store = try IsolatedDefaults()
        let defaults = store.defaults
        let selection = MockScenarioSelection(defaults: defaults)

        selection.select(.recoveryOverdue)

        #expect(selection.selected() == .recoveryOverdue)
        #expect(defaults.string(forKey: MockScenarioSelection.defaultsKey) == "recovery-overdue")

        selection.clear()

        #expect(selection.selected() == nil)
        #expect(defaults.string(forKey: MockScenarioSelection.defaultsKey) == nil)
    }

    @Test("a recorded value the build no longer knows is ignored rather than crashed on")
    func unknownRecordedValueIsIgnored() throws {
        let store = try IsolatedDefaults()
        store.defaults.set("a-scenario-from-the-future", forKey: MockScenarioSelection.defaultsKey)
        let selection = MockScenarioSelection(defaults: store.defaults)

        #expect(selection.selected() == nil, "a scenario this build cannot name is not a scenario")
    }
}
