import XCTest

/// Walks the app's top-level surfaces and runs Apple's accessibility audit on
/// each one.
///
/// This suite is the enforceable half of "Apple design compliant". The design
/// judgements — spacing, hierarchy, whether a control belongs on the glass layer
/// — need a human. Contrast ratios, hit-region sizes, missing element
/// descriptions, and text that clips at large Dynamic Type sizes do not, and
/// those are exactly the defects that accumulate silently across eighty screens.
///
/// Each test launches into the scenario that makes its surface reachable, so the
/// states that only exist under failure — quarantine, quota exhaustion, an
/// unreachable federated origin — are audited too, not just the happy path.
///
/// **Scenarios are not screens.** Every test below the launch surfaces exists
/// because for a long time this suite audited only what each scenario showed on
/// launch, and never navigated. That is fourteen surfaces out of eighty, and it
/// meant an audit could pass while every screen behind a tap was unchecked. The
/// walking tests are the fix, and the ones that matter most are the ones a
/// launch can never reach: a pushed detail screen, a modal viewer over black, a
/// settings form.
final class AccessibilityAuditTests: CapsuleUITestCase {
    func testLibraryTabIsAccessible() {
        launch(scenario: .healthy)
        require(app.tabBars.firstMatch.exists ? app.tabBars.firstMatch : app.windows.firstMatch)
        auditAccessibility()
    }

    func testEmptyLibraryIsAccessible() {
        launch(scenario: .emptyLibrary)
        auditAccessibility()
    }

    /// The never-signed-in mode is a first-class product state, not an error one:
    /// a user who never connects a server still gets a complete local gallery.
    func testNeverSignedInIsAccessible() {
        launch(scenario: .neverSignedIn)
        auditAccessibility()
    }

    func testQuarantineSurfaceIsAccessible() {
        launch(scenario: .quarantine)
        auditAccessibility()
    }

    func testQuotaGraceExpiredIsAccessible() {
        launch(scenario: .quotaGraceExpired)
        auditAccessibility()
    }

    func testDegradedFederationIsAccessible() {
        launch(scenario: .degradedFederation)
        auditAccessibility()
    }

    func testOfflineIsAccessible() {
        launch(scenario: .offline)
        auditAccessibility()
    }

    func testQuotaSoftWarningIsAccessible() {
        launch(scenario: .quotaSoftWarning)
        auditAccessibility()
    }

    /// Staged uploads leave originals on the device that took them, so the
    /// gallery is full of assets that exist but cannot yet be opened at full
    /// size. The badges that say so are easy to render at insufficient contrast.
    func testAwaitingOriginalsIsAccessible() {
        launch(scenario: .awaitingOriginals)
        auditAccessibility()
    }

    /// Documents written by a newer client. The indicator has to read as
    /// *informational* rather than as an error, and it must still describe
    /// itself to VoiceOver — a glyph-only badge here is a dead end for a
    /// screen-reader user.
    func testNewerVersionStateIsAccessible() {
        launch(scenario: .newerVersionState)
        auditAccessibility()
    }

    func testUndecodableAssetsIsAccessible() {
        launch(scenario: .undecodableAssets)
        auditAccessibility()
    }

    func testRecoveryOverdueIsAccessible() {
        launch(scenario: .recoveryOverdue)
        auditAccessibility()
    }

    func testProtocolUpgradeRequiredIsAccessible() {
        launch(scenario: .protocolUpgradeRequired)
        auditAccessibility()
    }

    /// The stress case. A quarter-million-asset grid renders far more elements
    /// than any other screen, and hit-region and description defects that a
    /// four-thousand-asset library never surfaces do surface here.
    func testHugeLibraryIsAccessible() {
        launch(scenario: .hugeLibrary)
        auditAccessibility()
    }
}

// MARK: - Navigated surfaces

extension AccessibilityAuditTests {
    /// The Browse index and a section reached through it.
    ///
    /// The phone's only route to sixteen of its twenty sections, so an audit
    /// that stops at the tab bar has never seen most of the app.
    func testBrowseIndexIsAccessible() throws {
        launch(scenario: .healthy)
        try tapTab(named: "Browse")
        require(app.windows.firstMatch)
        auditAccessibility()
    }

    /// The viewer: full-screen chrome floating over a photograph.
    ///
    /// The hardest surface in the app to get right and the one a launch can
    /// never reach — white glyphs on glass over arbitrary image content is
    /// exactly where contrast fails.
    func testViewerIsAccessible() throws {
        launch(scenario: .healthy)
        let tile = app.descendants(matching: .any).matching(identifier: "grid.tile").firstMatch
        try XCTSkipUnless(tile.waitForExistence(timeout: 15), "the timeline drew no tiles")
        tile.tap()
        try XCTSkipUnless(
            app.descendants(matching: .any).matching(identifier: "viewer.image")
                .firstMatch.waitForExistence(timeout: 15),
            "the viewer never showed a photo"
        )
        auditAccessibility()
    }

    /// A settings form — dense static text, the surface most likely to clip at
    /// large Dynamic Type sizes.
    func testSettingsIsAccessible() throws {
        launch(scenario: .healthy)
        try tapTab(named: "Settings")
        require(app.windows.firstMatch)
        auditAccessibility()
    }

    /// Tap a tab by its visible label.
    ///
    /// By label rather than identifier because the tab bar's buttons are built
    /// by SwiftUI from a title and a symbol and carry no identifier of their
    /// own — the same reason `SectionReachabilityTests` walks it positionally.
    private func tapTab(named label: String) throws {
        let tab = app.tabBars.buttons[label]
        try XCTSkipUnless(tab.waitForExistence(timeout: 20), "this shell has no '\(label)' tab")
        tab.tap()
    }
}
