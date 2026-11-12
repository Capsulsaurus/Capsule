import XCTest

/// The app under Xcode's two development pseudolanguages (i18n design doc, Layout →
/// Pseudo-locales), selected with the same launch arguments a scheme's "App Language"
/// option passes.
///
/// - Double-length: every localized string is shown twice, `Photos Photos`. A label that
///   is not doubled never went through the string catalog — a hardcoded string the
///   `i18n-guard` pattern scan missed.
/// - Right to left: the layout direction reaches the root, so the navigation bar's back
///   affordance sits at the trailing (right) edge; the left-to-right control asserts the
///   opposite.
///
/// The screens are reached through identifiers, as `SectionReachabilityTests` does: a
/// UI-test bundle cannot read the app's string catalog.
final class PseudolanguageRenderTests: CapsuleUITestCase {
    /// Xcode's "Double-Length Pseudolanguage".
    private static let doubleLength = ["-NSDoubleLocalizedStrings", "YES"]

    /// Xcode's "Right-to-Left Pseudolanguage".
    private static let rightToLeft = [
        "-AppleTextDirection", "YES",
        "-NSForceRightToLeftWritingDirection", "YES",
    ]

    /// Main screens whose every label is an app string: the Browse and Settings indexes,
    /// each reached from the tab bar. The library grid is left out on purpose — its
    /// labels are dates, which are formatted data rather than catalog strings.
    private static let mainScreens = ["browse.albums", "settings.section.language"]

    func testEveryVisibleStringIsCataloguedUnderTheDoubleLengthPseudolanguage() throws {
        launch(scenario: .healthy, arguments: Self.doubleLength)
        for marker in Self.mainScreens {
            try XCTSkipUnless(openTab(showing: marker), "no tab shows '\(marker)' in this shell")
            let labels = app.staticTexts.allElementsBoundByIndex
                .map { $0.label.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
            XCTAssertFalse(labels.isEmpty, "the screen with '\(marker)' shows no text")
            let unmarked = labels.filter { !Self.isData($0) && !Self.isDoubled($0) }
            XCTAssertEqual(unmarked, [], "strings not in the catalog on the screen with '\(marker)'")
        }
    }

    func testTheBackAffordanceMirrorsUnderTheRightToLeftPseudolanguage() throws {
        launch(scenario: .healthy, arguments: Self.rightToLeft)
        let back = try pushedBackButton()
        let window = app.windows.firstMatch.frame
        XCTAssertGreaterThan(back.frame.minX, window.midX, "back is not at the trailing (right) edge")
    }

    /// The control for the mirror: left to right, back sits at the leading (left) edge.
    func testTheBackAffordanceLeadsLeftToRight() throws {
        launch(scenario: .healthy)
        let back = try pushedBackButton()
        let window = app.windows.firstMatch.frame
        XCTAssertLessThan(back.frame.maxX, window.midX, "back is not at the leading (left) edge")
    }

    // MARK: Helpers

    /// Push a screen from the Browse index and return the navigation bar's back button.
    private func pushedBackButton() throws -> XCUIElement {
        try XCTSkipUnless(openTab(showing: "browse.albums"), "this shell has no Browse tab")
        element("browse.albums").tap()
        let back = app.navigationBars.buttons.firstMatch
        XCTAssertTrue(back.waitForExistence(timeout: 10), "Albums never pushed a screen")
        return back
    }

    /// Tap through the tab bar until a screen shows the element `identifier`.
    private func openTab(showing identifier: String) -> Bool {
        let bar = app.tabBars.firstMatch
        guard bar.waitForExistence(timeout: 20) else { return false }
        if element(identifier).exists { return true }
        for button in bar.buttons.allElementsBoundByIndex {
            button.tap()
            if element(identifier).waitForExistence(timeout: 3) { return true }
        }
        return false
    }

    private func element(_ identifier: String) -> XCUIElement {
        app.descendants(matching: .any)[identifier]
    }

    /// Whether `label` is a doubled string, `X X`, as the double-length pseudolanguage
    /// renders every catalog string.
    private static func isDoubled(_ label: String) -> Bool {
        let characters = Array(label)
        guard characters.count >= 3, characters.count % 2 == 1 else { return false }
        let half = characters.count / 2
        return characters[half] == " " && characters[..<half] == characters[(half + 1)...]
    }

    /// A label with no letters (a count, a size) is a formatted value, not an app string.
    private static func isData(_ label: String) -> Bool {
        !label.contains(where: \.isLetter)
    }
}
