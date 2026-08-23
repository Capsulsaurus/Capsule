import XCTest

/// Walks the app's own navigation surface and asserts each section presents a
/// real screen.
///
/// The failure this exists to catch is specific and quiet: a section that
/// navigates perfectly, renders something, and shows a placeholder where a
/// finished screen already exists three modules away. Nothing about that is
/// visible from a build, from a unit test of the screen itself, or from a
/// launch — only from walking in and looking.
///
/// The walk is written against *identifiers*, not labels, because a UI-test
/// bundle links the app as a target and so cannot read its string catalog. It
/// also does not assume which shell it is in: a regular-width run selects
/// sections from the sidebar, a compact one from the tab bar, and either way the
/// section that ends up showing announces itself as `section.<name>`.
final class SectionReachabilityTests: CapsuleUITestCase {
    /// Sections whose landing screen is still a placeholder.
    ///
    /// The mirror image of the unit suite's not-built list, kept to what a walk
    /// can actually reach. Finishing People means deleting a string here — and
    /// forgetting to fails ``testPlaceholderSectionsAreStillPlaceholders``,
    /// which is the point: the list shrinks because a test says so.
    private static let placeholderSections: Set<String> = ["people", "memories", "duplicates"]

    /// Every section the sidebar can offer, so a walk can name where it landed.
    private static let allSections = [
        "library", "browse", "memories", "duplicates", "trash", "hidden",
        "albums", "people", "places", "search",
        "transfers", "imports", "shares", "drops", "quarantine",
        "devices", "peers", "federation", "storage", "settings",
    ]

    /// The sections the phone's tab bar does not carry, and which Browse must
    /// therefore list. Mirrors `SidebarItem.browsable`, in its order.
    ///
    /// Hand-written rather than imported: this bundle drives the app through
    /// identifiers rather than linking its types, the same way the section list
    /// above does.
    private static let browsableSections = [
        "memories", "duplicates", "trash", "hidden",
        "albums", "people", "places",
        "transfers", "imports", "shares", "drops", "quarantine",
        "devices", "peers", "federation", "storage",
    ]

    /// Every section this shell can select lands on a real screen — unless it is
    /// one of the declared placeholders, in which case it must still be one.
    ///
    /// Asserted in both directions on purpose. "No placeholder anywhere" would
    /// pass just as well if the sweep silently selected nothing, and a list of
    /// known gaps that is never checked for staleness stops describing the app
    /// the first time one of them is filled in.
    func testEverySelectableSectionPresentsItsScreen() {
        launch(scenario: .healthy)
        var visited: Set<String> = []

        for selector in sectionSelectors() where selector.element.exists {
            selector.element.tap()
            guard let section = name(of: selector) else { continue }
            visited.insert(section)
            XCTAssertEqual(
                element("route.scaffold").waitForExistence(timeout: 2),
                Self.placeholderSections.contains(section),
                "section '\(section)' disagrees with the declared placeholder list"
            )
        }

        XCTAssertFalse(visited.isEmpty, "the sweep selected no section at all")
    }

    /// The declared placeholders really are placeholders.
    ///
    /// Separate from the sweep above because a placeholder section is not
    /// necessarily selectable in every shell — the iPhone tab bar carries four
    /// of nineteen — and "we could not reach it" must not read as "it is fine".
    func testPlaceholderSectionsAreStillPlaceholders() throws {
        launch(scenario: .healthy)
        let reachable = selectableSectionNames()
        let checkable = Self.placeholderSections.intersection(reachable)
        try XCTSkipIf(checkable.isEmpty, "this shell surfaces no placeholder section")

        for section in checkable {
            XCTAssertTrue(select(section), "section '\(section)' became unselectable")
            XCTAssertTrue(
                element("route.scaffold").waitForExistence(timeout: 5),
                "section '\(section)' is no longer a placeholder — remove it from placeholderSections"
            )
        }
    }

    /// The settings index lists its screens, and opening one shows a screen.
    ///
    /// Settings is the section most likely to regress into a dead end, because
    /// its eighteen screens are reached through an index rather than through the
    /// sidebar: a broken index makes all eighteen unreachable at once while the
    /// sidebar still looks perfectly healthy.
    func testSettingsIndexOpensItsScreens() throws {
        launch(scenario: .healthy)
        try XCTSkipUnless(select("settings"), "no Settings surface in this shell")

        for section in ["account", "appearance", "language", "sync"] {
            let row = element("settings.section.\(section)")
            guard row.waitForExistence(timeout: 5) else {
                XCTFail("the settings index has no row for '\(section)'")
                continue
            }
            row.tap()
            XCTAssertFalse(
                element("route.scaffold").waitForExistence(timeout: 2),
                "settings screen '\(section)' still presents the placeholder"
            )
            goBack()
        }
    }

    // MARK: Walking

    /// Everything that can select a section, in this shell.
    ///
    /// The sidebar's rows carry identifiers; the tab bar's buttons are built by
    /// SwiftUI from a title and a symbol and carry none, so they are taken
    /// positionally and identified after the fact by what they present.
    private func sectionSelectors() -> [SectionSelector] {
        waitForSectionSurface()
        let rows = Self.allSections
            .map { SectionSelector(name: $0, element: element("sidebar.\($0)")) }
            .filter { $0.element.exists }
        if !rows.isEmpty { return rows }

        let bar = app.tabBars.firstMatch
        guard bar.waitForExistence(timeout: 15) else { return [] }
        return bar.buttons.allElementsBoundByIndex.map { SectionSelector(name: nil, element: $0) }
    }

    /// Wait until the shell has drawn something that can select a section.
    ///
    /// Without this the probe runs against a window that exists but is still
    /// empty, every row reads as absent, and a shell with nineteen sections
    /// looks exactly like a shell with none.
    private func waitForSectionSurface() {
        _ = app.windows.firstMatch.waitForExistence(timeout: 20)
        let deadline = Date().addingTimeInterval(15)
        repeat {
            if app.tabBars.firstMatch.exists { return }
            if Self.allSections.contains(where: { element("sidebar.\($0)").exists }) { return }
        } while Date() < deadline
    }

    /// One way into a section, and the section's name when the surface it came
    /// from already knew it.
    private struct SectionSelector {
        /// The sidebar knows which row it drew; a tab-bar button does not carry
        /// one, so its section is read off the page it presents instead.
        let name: String?
        let element: XCUIElement
    }

    /// Every section the phone's tab bar cannot carry is reachable through
    /// Browse — which is the only reason Browse exists.
    ///
    /// Fifteen sections were unreachable on iPhone before this, and no test
    /// failed: the catalog derived an `overflow` list that no shell rendered,
    /// and the suite that checked the placement partition never checked that
    /// anything *drew* the second half of it. A partition is not reachability.
    func testBrowseIndexReachesEverySection() throws {
        launch(scenario: .healthy)
        try XCTSkipUnless(select("browse"), "this shell has no Browse tab")

        for section in Self.browsableSections {
            let row = element("browse.\(section)")
            guard row.waitForExistence(timeout: 5) else {
                XCTFail("the Browse index has no row for '\(section)'")
                continue
            }
            row.tap()
            XCTAssertEqual(
                element("route.scaffold").waitForExistence(timeout: 2),
                Self.placeholderSections.contains(section),
                "'\(section)' disagrees with the declared placeholder list"
            )
            dismissSystemAuthPrompt()
            goBack()
        }
    }

    /// A map pin previews its photos in place, on a device with no pointer.
    ///
    /// Hover is the pointer affordance and there is no hover on a phone, so the
    /// first tap has to do that job — otherwise the priority platform is the
    /// one that loses the feature. The second tap, on the fan itself, opens the
    /// place.
    func testMapPinPreviewsItsPhotosOnTap() throws {
        launch(scenario: .healthy)
        try XCTSkipUnless(select("browse"), "this shell has no Browse tab")
        let places = element("browse.places")
        try XCTSkipUnless(places.waitForExistence(timeout: 5), "Browse has no Places row")
        places.tap()

        let pin = app.descendants(matching: .any).matching(identifier: "place.pin").firstMatch
        XCTAssertTrue(pin.waitForExistence(timeout: 10), "the map drew no pins")
        pin.tap()

        let preview = app.descendants(matching: .any).matching(identifier: "place.preview").firstMatch
        XCTAssertTrue(
            preview.waitForExistence(timeout: 10),
            "tapping a pin showed no photo preview"
        )
    }

    /// Dismiss the system authentication alert, if one is up.
    ///
    /// Hidden is gated on fresh local authentication, and a simulator with no
    /// enrolled biometry falls back to a passcode alert owned by SpringBoard —
    /// not by the app. Left standing it swallows every later tap, so the sweep
    /// finds no rows and reports the *next* section missing rather than this
    /// one being modal. That the prompt appears at all is the gate working;
    /// this only gets the walk past it.
    private func dismissSystemAuthPrompt() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for label in ["Cancel", "Fallback"] {
            let button = springboard.buttons[label]
            if button.exists, button.isHittable {
                button.tap()
                return
            }
        }
    }

    /// Which section a selector landed on.
    private func name(of selector: SectionSelector) -> String? {
        selector.name ?? visibleSection()
    }

    /// The names of the sections this shell can select, by walking it once.
    private func selectableSectionNames() -> Set<String> {
        var names: Set<String> = []
        for selector in sectionSelectors() where selector.element.exists {
            selector.element.tap()
            if let section = name(of: selector) { names.insert(section) }
        }
        return names
    }

    /// Select one section by name, reporting whether this shell offers it.
    @discardableResult
    private func select(_ section: String) -> Bool {
        for selector in sectionSelectors() where selector.element.exists {
            selector.element.tap()
            if name(of: selector) == section { return true }
        }
        return false
    }

    /// Which section is on screen, once it has settled. Only the compact shell
    /// needs this: its tab bar buttons are built by SwiftUI from a title and a
    /// symbol and carry no identifier of their own.
    private func visibleSection() -> String? {
        let deadline = Date().addingTimeInterval(5)
        repeat {
            for section in Self.allSections
                where element("section.\(section)").exists {
                return section
            }
        } while Date() < deadline
        return nil
    }

    /// One element by identifier, anywhere in the app.
    ///
    /// SwiftUI leaves a row's identifier on every element the row produced — a
    /// label yields both an image and a text — so the text is preferred: it is
    /// the one that is reliably hittable, and tapping either selects the row.
    /// `matching(identifier:).firstMatch` is deliberately not used; it does not
    /// resolve through a collection view's cells the way the subscript does.
    private func element(_ identifier: String) -> XCUIElement {
        let text = app.staticTexts[identifier]
        return text.exists ? text : app.descendants(matching: .any)[identifier]
    }

    /// Return to the previous screen.
    ///
    /// Prefers the navigation bar's own control and falls back to the
    /// interactive edge-swipe, because which of the two exists depends on the
    /// shell and on how the pushed screen configures its bar — and this suite
    /// asserts where navigation *goes*, not how the chrome is drawn.
    private func goBack() {
        let back = app.navigationBars.buttons.firstMatch
        if back.exists, back.isHittable {
            back.tap()
            return
        }
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.5))
        let target = app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5))
        edge.press(forDuration: 0.05, thenDragTo: target)
    }
}
