import CapsuleFoundation
import CapsuleMock
import Observation

// MARK: - AdvancedSettingsModel

/// Drives the Advanced screen's mock-scenario switcher.
///
/// Around thirty screens are unreachable from a healthy library, and this
/// switcher is how they are reached by hand without relaunching from Xcode with
/// a launch argument. A scenario is chosen at launch — ``MockEnvironment``
/// builds the whole world from one — so a choice here is **recorded for the
/// next launch** through ``MockScenarioSelection`` and never applied in place.
/// The composition root reads it at launch, and a `-mock-scenario` launch
/// argument still wins over it.
///
/// The switcher exists only when this build runs against a mock world. With no
/// ``SettingsEnvironment/activeScenarioName`` the screen has nothing to offer
/// and reports ``SettingsPhase/empty`` rather than drawing an empty picker.
@MainActor
@Observable
public final class AdvancedSettingsModel {
    /// The scenario this launch is running, as the raw name the environment
    /// carries. `nil` in a build wired to a real server.
    public let activeScenarioName: String?

    /// The scenario recorded for the next launch, if one is.
    public private(set) var nextLaunchScenario: MockScenario?

    /// Every scenario the switcher offers, in declaration order.
    public let scenarios: [MockScenario] = MockScenario.allCases

    /// The scenario the next launch runs when no override is recorded: the one
    /// this launch's arguments name, or ``MockScenario/healthy`` for a launch
    /// with none.
    ///
    /// This is not ``activeScenarioName``: a launch with no arguments may be
    /// running a world it read from a saved choice, and once that choice is
    /// cleared the next argument-less launch runs the healthy world instead.
    public let launchDefaultScenario: MockScenario

    private let selection: MockScenarioSelection

    /// - Parameter launchArguments: this launch's process arguments. The next
    ///   launch is assumed to receive the same ones, as an Xcode scheme or a
    ///   home-screen launch does.
    public init(
        activeScenarioName: String?,
        launchArguments: [String],
        selection: MockScenarioSelection
    ) {
        self.activeScenarioName = activeScenarioName
        launchDefaultScenario = MockScenario.resolve(fromArguments: launchArguments)
        self.selection = selection
        nextLaunchScenario = selection.selected()
    }

    /// Whether the scenario switcher is shown at all.
    public var showsScenarioSwitcher: Bool {
        activeScenarioName != nil
    }

    /// The screen's phase: there is nothing to show without a mock world.
    public var phase: SettingsPhase {
        showsScenarioSwitcher ? .ready : .empty
    }

    /// The scenario the picker shows as chosen: the recorded override, or else
    /// the ``launchDefaultScenario`` the next launch falls back to.
    public var pickedScenario: MockScenario {
        nextLaunchScenario ?? launchDefaultScenario
    }

    /// Whether the next launch will run a different world from this one — the
    /// condition for showing the relaunch notice.
    public var appliesOnNextLaunch: Bool {
        pickedScenario.rawValue != activeScenarioName
    }

    /// Record a scenario for the next launch.
    ///
    /// Refused without a mock world: a real build must never write an override
    /// that a later mock build would silently pick up.
    public func choose(_ scenario: MockScenario) {
        guard showsScenarioSwitcher else {
            CapsuleLog.app.notice(
                "mock scenario choice refused: no mock world is active"
            )
            return
        }
        selection.select(scenario)
        nextLaunchScenario = scenario
        CapsuleLog.app.info(
            "mock scenario recorded for next launch: \(scenario.rawValue, privacy: .public)"
        )
    }

    /// Forget the override, so the next launch follows its launch arguments.
    public func clearOverride() {
        selection.clear()
        nextLaunchScenario = nil
        CapsuleLog.app.info(
            "mock scenario override cleared; next launch runs: \(self.launchDefaultScenario.rawValue, privacy: .public)"
        )
    }
}
