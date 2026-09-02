import XCTest

/// The load-bearing navigation paths, driven end to end against the mocks.
///
/// These are deliberately shallow and few. A UI test that asserts on layout is a
/// test that fails every time the design improves; these assert only that a path
/// through the app *exists and completes* — the thing that silently breaks when a
/// route is rewired and nobody notices until a user reports a dead end.
final class LaunchFlowTests: CapsuleUITestCase {
    /// The app must reach a usable library with no account and no network. This
    /// is the offline-first contract: sync is an addition to a working product,
    /// never its precondition.
    func testLaunchesIntoAUsableLibraryWithoutAnAccount() {
        let app = launch(scenario: .neverSignedIn)
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 15))
    }

    /// A cold launch against a very large library must still present the grid
    /// promptly — the virtualized timeline is sized for hundreds of thousands of
    /// assets, and a regression to a materializing implementation shows up here
    /// as a launch that never settles.
    func testLaunchesPromptlyAgainstAHugeLibrary() {
        let app = launch(scenario: .hugeLibrary)
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 20))
    }

    /// Every scenario must produce a running app.
    ///
    /// Deliberately the weakest possible assertion, and deliberately exhaustive.
    /// Most of these worlds exist to make an *error* surface reachable, which
    /// means their composition roots are the least exercised code in the app —
    /// a mock that traps or a force-unwrap on a state only this scenario
    /// produces would otherwise be found by a reviewer, by hand, or not at all.
    /// Driving the enum rather than a hand-written list is the point: a new
    /// scenario is covered the moment it is added.
    func testEveryScenarioLaunches() {
        for scenario in MockScenarioName.allCases {
            let app = launch(scenario: scenario)
            XCTAssertTrue(
                app.windows.firstMatch.waitForExistence(timeout: 20),
                "scenario '\(scenario.rawValue)' never presented a window"
            )
            XCTAssertEqual(
                app.state, .runningForeground,
                "scenario '\(scenario.rawValue)' left the app in state \(app.state.rawValue)"
            )
            app.terminate()
        }
    }
}
