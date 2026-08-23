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
