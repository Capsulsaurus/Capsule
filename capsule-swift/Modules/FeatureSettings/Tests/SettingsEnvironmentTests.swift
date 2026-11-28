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
