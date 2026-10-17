import XCTest

/// Base class for Capsule's UI automation.
///
/// XCTest appears here and **only** here. The client contract pins swift-testing
/// as the sole unit/smoke framework on Apple platforms and sanctions XCTest
/// exclusively inside XCUITest bundles, where no swift-testing analogue exists.
/// Every other test target in this project is `@Suite`/`@Test`.
///
/// The class exists to make two things impossible to forget: launching with an
/// explicit mock scenario, and running the accessibility audit. A screen that is
/// only reachable in one scenario is still audited, because the audit runs
/// against whatever is on screen when it is called.
/// `@MainActor` on the base class rather than on the one method that needs it:
/// the audit hands its issues back on the main actor, and annotating a single
/// helper would make every call site in every subclass implicitly async. XCUITest
/// bodies already run on the main thread, so this states what was already true.
@MainActor
class CapsuleUITestCase: XCTestCase {
    /// The app under test, launched by ``launch(scenario:)``.
    private(set) var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        // A UI test that limps on after a failed assertion produces a cascade of
        // misleading follow-on failures; the first one is the real signal.
        continueAfterFailure = false
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    /// Launch the app with a deterministic mock scenario.
    ///
    /// The app reads `-mock-scenario` at startup and composes `CapsuleMock`
    /// accordingly, so a test can put the UI into a state — quarantined assets, a
    /// quota grace window, an unreachable federated origin — that would otherwise
    /// need a server, a network fault, and a fortnight of waiting.
    @discardableResult
    func launch(scenario: MockScenarioName = .healthy) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-mock-scenario", scenario.rawValue]
        // Deterministic UI: no first-run coach marks, no animation timing races.
        app.launchArguments += ["-capsule-ui-testing", "1"]
        app.launch()
        self.app = app
        return app
    }

    /// Run Apple's accessibility audit against the current screen.
    ///
    /// This is the objective, automatable part of "Apple design compliant" —
    /// contrast, hit-region size, element descriptions, and Dynamic Type
    /// clipping, checked by the same engine Xcode's Accessibility Inspector uses.
    ///
    /// - Parameter excluding: audit types to skip, with a comment saying why.
    ///   Skipping a check silently is how an accessibility regression ships.
    func auditAccessibility(
        excluding: XCUIAccessibilityAuditType = [],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        do {
            try app.performAccessibilityAudit(for: .all.subtracting(excluding)) { issue in
                // Report rather than swallow. The throwing form fails with
                // "Contrast failed" and nothing else — not which element, not
                // what ratio, not where on screen — which makes a failure
                // impossible to act on and, in practice, a failure nobody runs.
                XCTFail(
                    """
                    \(Self.name(of: issue.auditType)): \(issue.compactDescription)
                      element: \(issue.element?.debugDescription ?? "unknown")
                      detail:  \(issue.detailedDescription)
                    """,
                    file: file,
                    line: line
                )
                return true
            }
        } catch {
            XCTFail("Accessibility audit could not run: \(error)", file: file, line: line)
        }
    }

    /// A readable name for an audit type, which is an `OptionSet` and prints as
    /// a raw value otherwise.
    private static func name(of type: XCUIAccessibilityAuditType) -> String {
        var names: [String] = []
        if type.contains(.contrast) { names.append("contrast") }
        if type.contains(.elementDetection) { names.append("element-detection") }
        if type.contains(.hitRegion) { names.append("hit-region") }
        if type.contains(.sufficientElementDescription) { names.append("element-description") }
        if type.contains(.dynamicType) { names.append("dynamic-type") }
        if type.contains(.textClipped) { names.append("text-clipped") }
        if type.contains(.trait) { names.append("trait") }
        return names.isEmpty ? "audit" : names.joined(separator: "+")
    }

    /// Wait for an element, failing with a useful message rather than a bare
    /// `false` assertion.
    @discardableResult
    func require(
        _ element: XCUIElement,
        timeout: TimeInterval = 10,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> XCUIElement {
        guard element.waitForExistence(timeout: timeout) else {
            XCTFail("Element never appeared: \(element)", file: file, line: line)
            return element
        }
        return element
    }
}

/// The mock scenarios the UI tests can launch into.
///
/// Mirrors `CapsuleMock.MockScenario`. It is duplicated rather than shared
/// because a UI-test bundle links the app as a *target*, not as a library, so it
/// cannot import the app's modules — the raw values are the contract, and a
/// mismatch fails loudly at launch rather than silently testing the wrong state.
enum MockScenarioName: String, CaseIterable {
    case healthy
    case emptyLibrary = "empty-library"
    case neverSignedIn = "never-signed-in"
    case offline
    case hugeLibrary = "huge-library"
    case quotaSoftWarning = "quota-soft-warning"
    case quotaGraceExpired = "quota-grace-expired"
    case quarantine
    case degradedFederation = "degraded-federation"
    case awaitingOriginals = "awaiting-originals"
    case newerVersionState = "newer-version-state"
    case undecodableAssets = "undecodable-assets"
    case recoveryOverdue = "recovery-overdue"
    case protocolUpgradeRequired = "protocol-upgrade-required"
}
