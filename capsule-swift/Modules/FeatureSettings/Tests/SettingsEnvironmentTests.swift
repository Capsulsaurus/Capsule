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

// MARK: - IsolatedDefaults

/// An isolated defaults suite, torn down when the test ends: a settings test
/// must never read or write the machine's own preferences.
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

// MARK: - MockScenarioSelectionTests

/// A scenario is chosen at launch, so switching one cannot take effect in
/// place. The switcher records the choice; it does not pretend to rebuild a
/// graph it does not own.
@Suite("The scenario switcher records a choice for the next launch")
@MainActor
struct MockScenarioSelectionTests {
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

    /// The other half of the contract: what the switcher writes is what the
    /// launch path reads, and a launch argument still wins over it.
    @Test("a recorded choice is the next launch's world unless the launch names one")
    func recordedChoiceReachesTheLaunchPath() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        selection.select(.quotaGraceExpired)
        let persisted = selection.selected()

        #expect(MockScenario.resolve(fromArguments: ["app"], persisted: persisted) == .quotaGraceExpired)
        #expect(
            MockScenario.resolve(
                fromArguments: ["app", MockScenario.launchArgument, "offline"],
                persisted: persisted
            ) == .offline,
            "a launch argument wins over the persisted override"
        )
        #expect(
            MockScenario.resolve(
                fromArguments: ["app", MockScenario.launchArgument, "typo"],
                persisted: persisted
            ) == .healthy,
            "a named but unknown scenario is the healthy world, not the persisted one"
        )
        #expect(
            MockScenario.resolve(fromArguments: ["app", MockScenario.launchArgument], persisted: persisted)
                == .healthy
        )
        #expect(MockScenario.resolve(fromArguments: ["app"], persisted: nil) == .healthy)
    }
}

// MARK: - AdvancedSettingsModelTests

/// The Advanced screen's switcher: shown only against a mock world, and every
/// choice is recorded for the next launch rather than applied in place.
@Suite("The Advanced screen records a scenario for the next launch")
@MainActor
struct AdvancedSettingsModelTests {
    @Test("a build with no mock world shows no switcher and writes nothing")
    func noMockWorldNoSwitcher() throws {
        let store = try IsolatedDefaults()
        let model = AdvancedSettingsModel(
            activeScenarioName: nil,
            launchArguments: ["app"],
            selection: MockScenarioSelection(defaults: store.defaults)
        )

        #expect(!model.showsScenarioSwitcher)
        #expect(model.phase == .empty)

        model.choose(.quarantine)

        #expect(model.nextLaunchScenario == nil)
        #expect(store.defaults.string(forKey: MockScenarioSelection.defaultsKey) == nil)
    }

    @Test("a mock world launched by argument shows the switcher on the scenario it is running")
    func mockWorldShowsSwitcher() throws {
        let store = try IsolatedDefaults()
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.offline.rawValue,
            launchArguments: ["app", MockScenario.launchArgument, MockScenario.offline.rawValue],
            selection: MockScenarioSelection(defaults: store.defaults)
        )

        #expect(model.showsScenarioSwitcher)
        #expect(model.phase == .ready)
        #expect(model.pickedScenario == .offline)
        #expect(!model.appliesOnNextLaunch)
        #expect(model.scenarios == MockScenario.allCases)
        #expect(model.launchArgumentNamesScenario)
        #expect(!model.canChooseScenario, "the argument wins, so the picker is not offered for choosing")
    }

    @Test("an argument-less mock launch lets the switcher choose")
    func argumentLessLaunchCanChoose() throws {
        let store = try IsolatedDefaults()
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.healthy.rawValue,
            launchArguments: ["app"],
            selection: MockScenarioSelection(defaults: store.defaults)
        )

        #expect(!model.launchArgumentNamesScenario)
        #expect(model.canChooseScenario)
    }

    @Test("a launch that names its world refuses a choice and shows no relaunch notice")
    func argumentLaunchRefusesChoosing() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.offline.rawValue,
            launchArguments: ["app", MockScenario.launchArgument, MockScenario.offline.rawValue],
            selection: selection
        )

        model.choose(.quarantine)

        #expect(model.nextLaunchScenario == nil)
        #expect(selection.selected() == nil, "nothing is written that the next launch would ignore")
        #expect(model.pickedScenario == .offline)
        #expect(!model.appliesOnNextLaunch)
    }

    @Test("a mistyped launch argument still names the world, as the launch path reads it")
    func mistypedArgumentStillWins() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        selection.select(.quarantine)
        let arguments = ["app", MockScenario.launchArgument, "typo"]
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.healthy.rawValue,
            launchArguments: arguments,
            selection: selection
        )

        #expect(model.launchArgumentNamesScenario)
        #expect(!model.canChooseScenario)
        #expect(model.pickedScenario == .healthy)
        #expect(model.pickedScenario == MockScenario.resolve(fromArguments: arguments, persisted: selection.selected()))
        #expect(!model.appliesOnNextLaunch)
    }

    @Test("choosing a different scenario records it and says it applies next launch")
    func choosingRecordsForNextLaunch() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.healthy.rawValue,
            launchArguments: ["app"],
            selection: selection
        )

        model.choose(.recoveryOverdue)

        #expect(model.nextLaunchScenario == .recoveryOverdue)
        #expect(model.pickedScenario == .recoveryOverdue)
        #expect(model.appliesOnNextLaunch)
        #expect(selection.selected() == .recoveryOverdue)
    }

    @Test("choosing the running scenario records it without a relaunch notice")
    func choosingTheRunningScenarioNeedsNoNotice() throws {
        let store = try IsolatedDefaults()
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.quarantine.rawValue,
            launchArguments: ["app"],
            selection: MockScenarioSelection(defaults: store.defaults)
        )

        model.choose(.quarantine)

        #expect(model.nextLaunchScenario == .quarantine)
        #expect(!model.appliesOnNextLaunch)
    }

    @Test("an override recorded earlier is shown when the screen opens")
    func earlierOverrideIsShown() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        selection.select(.hugeLibrary)

        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.healthy.rawValue,
            launchArguments: ["app"],
            selection: selection
        )

        #expect(model.nextLaunchScenario == .hugeLibrary)
        #expect(model.pickedScenario == .hugeLibrary)
        #expect(model.appliesOnNextLaunch)
    }

    @Test("clearing the override hands the next launch back to its arguments")
    func clearingForgetsTheOverride() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.healthy.rawValue,
            launchArguments: ["app"],
            selection: selection
        )
        model.choose(.offline)

        model.clearOverride()

        #expect(model.nextLaunchScenario == nil)
        #expect(model.pickedScenario == .healthy)
        #expect(!model.appliesOnNextLaunch)
        #expect(selection.selected() == nil)
    }

    /// The running world came from the saved choice, not from an argument, so
    /// clearing it hands the next argument-less launch to the healthy world —
    /// and the screen must say so rather than keep showing the running one.
    @Test("clearing a saved choice the running world came from shows the healthy world next")
    func clearingTheSavedRunningWorldShowsHealthyNext() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        selection.select(.quarantine)
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.quarantine.rawValue,
            launchArguments: ["app"],
            selection: selection
        )

        #expect(model.pickedScenario == .quarantine)
        #expect(!model.appliesOnNextLaunch)

        model.clearOverride()

        #expect(model.launchDefaultScenario == .healthy)
        #expect(model.pickedScenario == .healthy)
        #expect(model.appliesOnNextLaunch, "the next launch runs healthy, not quarantine")
        #expect(MockScenario.resolve(fromArguments: ["app"], persisted: selection.selected()) == .healthy)
    }

    /// An override an earlier argument-less launch recorded is still on the
    /// device, but this launch's argument wins over it, and so will the next
    /// launch's: the picker shows the argument's world and no relaunch notice,
    /// both before and after the override is cleared.
    @Test("an override recorded earlier yields to a launch that names its world")
    func clearingUnderALaunchArgumentKeepsItsWorld() throws {
        let store = try IsolatedDefaults()
        let selection = MockScenarioSelection(defaults: store.defaults)
        selection.select(.quarantine)
        let arguments = ["app", MockScenario.launchArgument, MockScenario.offline.rawValue]
        let model = AdvancedSettingsModel(
            activeScenarioName: MockScenario.offline.rawValue,
            launchArguments: arguments,
            selection: selection
        )

        #expect(model.nextLaunchScenario == .quarantine, "the recorded override is kept and can be cleared")
        #expect(model.pickedScenario == .offline, "the argument wins over the recorded override")
        #expect(model.pickedScenario == MockScenario.resolve(fromArguments: arguments, persisted: selection.selected()))
        #expect(!model.appliesOnNextLaunch, "the next launch, with the same arguments, runs this world again")

        model.clearOverride()

        #expect(selection.selected() == nil)

        #expect(model.launchDefaultScenario == .offline)
        #expect(model.pickedScenario == .offline)
        #expect(!model.appliesOnNextLaunch)
    }

    @Test("a running scenario name this build cannot parse is shown as the healthy world")
    func unparseableRunningNameFallsBack() throws {
        let store = try IsolatedDefaults()
        let model = AdvancedSettingsModel(
            activeScenarioName: "a-scenario-from-the-future",
            launchArguments: ["app"],
            selection: MockScenarioSelection(defaults: store.defaults)
        )

        #expect(model.showsScenarioSwitcher)
        #expect(model.pickedScenario == .healthy)
    }
}
