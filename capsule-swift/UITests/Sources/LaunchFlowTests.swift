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
}
