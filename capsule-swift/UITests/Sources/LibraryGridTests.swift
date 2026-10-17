import XCTest

/// Asserts the library grid is the continuous, dateable surface it was rebuilt
/// to be.
///
/// Both assertions here exist because a screenshot said one thing and the code
/// said another. Removing the day headers made the grid continuous, but it also
/// removed the only thing on screen that said *when* — and the replacement, a
/// date in the navigation bar, was invisible for a while because the
/// aggregation-level picker was sitting in the `.principal` slot, which is the
/// slot the title occupies. Everything compiled, every unit test passed, and the
/// feature did not exist.
final class LibraryGridTests: CapsuleUITestCase {
    /// Scrolling the library replaces its title with where you are in time.
    ///
    /// The grid draws no day headers, so this navigation-bar date is the whole
    /// of the app's answer to "when am I looking at". If it silently stops
    /// updating, a quarter-million-photo library becomes unnavigable and nothing
    /// else fails.
    func testScrollingTheLibraryShowsWhereYouAreInTime() {
        let app = launch(scenario: .hugeLibrary)

        let tile = app.descendants(matching: .any).matching(identifier: "grid.tile").firstMatch
        XCTAssertTrue(tile.waitForExistence(timeout: 30), "the library never drew a tile")

        // Far enough that the leading tile is certainly a different day.
        for _ in 0 ..< 12 {
            app.swipeUp(velocity: .fast)
        }

        // The title starts as the library's own name and becomes a date. Either
        // spelling of "still the name" is a failure: the date never arrived.
        let navigationBar = app.navigationBars.firstMatch
        XCTAssertTrue(navigationBar.waitForExistence(timeout: 10))

        let titles = navigationBar.staticTexts.allElementsBoundByIndex.map(\.label)
        XCTAssertFalse(titles.isEmpty, "the navigation bar showed no title at all")
        XCTAssertTrue(
            titles.contains(where: Self.looksLikeADate),
            "expected a date in the navigation bar after scrolling, got \(titles)"
        )
    }

    /// The aggregation picker is reachable without going through the navigation
    /// bar, which is what freed the title to carry the date.
    func testTheAggregationPickerIsOutsideTheNavigationBar() {
        let app = launch(scenario: .healthy)

        let tile = app.descendants(matching: .any).matching(identifier: "grid.tile").firstMatch
        XCTAssertTrue(tile.waitForExistence(timeout: 30), "the library never drew a tile")

        // A segmented control with all three levels, and not inside the bar.
        let allSegment = app.buttons["All"]
        XCTAssertTrue(allSegment.waitForExistence(timeout: 10), "no All segment on screen")
        XCTAssertFalse(
            app.navigationBars.buttons["All"].exists,
            "the level picker is back in the navigation bar, which hides the title"
        )
        XCTAssertTrue(app.buttons["Years"].exists)
        XCTAssertTrue(app.buttons["Months"].exists)
    }

    /// A label carrying a month name and a four-digit year.
    ///
    /// Matched loosely on purpose: this bundle cannot read the app's string
    /// catalog, so the exact rendering is not knowable here, and pinning it
    /// would make the test fail on a locale change rather than on a regression.
    private static func looksLikeADate(_ label: String) -> Bool {
        let months = [
            "January", "February", "March", "April", "May", "June",
            "July", "August", "September", "October", "November", "December",
        ]
        let hasYear = label.range(of: #"\b\d{4}\b"#, options: .regularExpression) != nil
        return hasYear && months.contains { label.localizedCaseInsensitiveContains($0) }
    }
}
