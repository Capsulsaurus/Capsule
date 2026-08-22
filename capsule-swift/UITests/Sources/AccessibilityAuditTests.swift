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
}
