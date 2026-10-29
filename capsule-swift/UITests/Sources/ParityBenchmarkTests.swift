import XCTest

/// The responsiveness parity benchmark: Capsule and Photos, on the same device, measured the
/// same way (Clients - Responsiveness).
///
/// Each test is named `test<Client><Measure>`, which is the contract `xtask bench-apps import`
/// maps onto the normalized metrics; renaming one without the importer fails the import instead
/// of dropping the number. Capsule launches into the `huge-library` mock scenario (250,000
/// synthetic assets, above the contract's 100,000); Photos shows whatever library the bench
/// device holds, which the bench runbook in `mise-tasks/bench-apps` prepares.
///
/// - `ColdStartToFirstThumbnail` - launch until the first grid thumbnail exists.
/// - `Scroll` - fling the grid; the system scroll signposts give frame rate and hitches, for
///   either app, without any cooperation from it.
/// - `FullResolutionOpen` - tap a thumbnail until the viewer's image exists.
///
/// Every duration is an `XCTClockMetric` around the same XCUITest calls for both apps, so the
/// harness overhead is the same on both sides of the comparison. Input-to-frame latency needs a
/// signpost inside the app, which this bundle cannot add; it is not measured here.
///
/// Photos' element identifiers are private and change between releases, so the viewer anchor
/// is supplied with the device: `TEST_RUNNER_PLATFORM_VIEWER_ID` (xcodebuild hands it to this
/// process as `PLATFORM_VIEWER_ID`). A Photos test that needs it is skipped when it is unset or empty.
final class ParityBenchmarkTests: CapsuleUITestCase {
    private static let photosBundle = "com.apple.mobileslideshow"
    private static let iterations = 5
    private static let timeout: TimeInterval = 30

    // MARK: Capsule

    func testCapsuleColdStartToFirstThumbnail() {
        coldStart(capsule(), thumbnail: capsuleTile)
    }

    func testCapsuleScroll() {
        scroll(capsule(), thumbnail: capsuleTile)
    }

    func testCapsuleFullResolutionOpen() {
        fullResolutionOpen(capsule(), thumbnail: capsuleTile) { app in
            app.descendants(matching: .any).matching(identifier: "viewer.image").firstMatch
        }
    }

    // MARK: Photos

    func testPlatformAppColdStartToFirstThumbnail() {
        coldStart(photos(), thumbnail: photosTile)
    }

    func testPlatformAppScroll() {
        scroll(photos(), thumbnail: photosTile)
    }

    func testPlatformAppFullResolutionOpen() throws {
        guard let identifier = ProcessInfo.processInfo.environment["PLATFORM_VIEWER_ID"], !identifier.isEmpty else {
            throw XCTSkip("set TEST_RUNNER_PLATFORM_VIEWER_ID to Photos' viewer element for this device")
        }
        fullResolutionOpen(photos(), thumbnail: photosTile) { app in
            app.descendants(matching: .any).matching(identifier: identifier).firstMatch
        }
    }

    // MARK: Measures

    /// Capsule, launched into the scale scenario, with nothing between launch and the grid.
    private func capsule() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-mock-scenario", MockScenarioName.hugeLibrary.rawValue]
        return app
    }

    private func photos() -> XCUIApplication {
        XCUIApplication(bundleIdentifier: Self.photosBundle)
    }

    private func capsuleTile(_ app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: "grid.tile").firstMatch
    }

    /// Photos' library grid is a collection view; its first cell is the first thumbnail.
    private func photosTile(_ app: XCUIApplication) -> XCUIElement {
        app.cells.firstMatch
    }

    private func options() -> XCTMeasureOptions {
        let options = XCTMeasureOptions()
        options.iterationCount = Self.iterations
        // Each iteration relaunches the app before timing starts; only the measured span counts.
        options.invocationOptions = [.manuallyStart, .manuallyStop]
        return options
    }

    private func coldStart(_ app: XCUIApplication, thumbnail: (XCUIApplication) -> XCUIElement) {
        measure(metrics: [XCTClockMetric()], options: options()) {
            app.terminate()
            startMeasuring()
            app.launch()
            let appeared = thumbnail(app).waitForExistence(timeout: Self.timeout)
            stopMeasuring()
            XCTAssertTrue(appeared, "the library never drew a thumbnail")
        }
    }

    private func scroll(_ app: XCUIApplication, thumbnail: (XCUIApplication) -> XCUIElement) {
        app.launch()
        XCTAssertTrue(thumbnail(app).waitForExistence(timeout: Self.timeout), "the library never drew a thumbnail")
        let options = XCTMeasureOptions()
        options.iterationCount = Self.iterations
        measure(metrics: [XCTOSSignpostMetric.scrollDecelerationMetric], options: options) {
            app.swipeUp(velocity: .fast)
        }
    }

    private func fullResolutionOpen(
        _ app: XCUIApplication,
        thumbnail: (XCUIApplication) -> XCUIElement,
        viewer: (XCUIApplication) -> XCUIElement
    ) {
        measure(metrics: [XCTClockMetric()], options: options()) {
            app.terminate()
            app.launch()
            let tile = thumbnail(app)
            guard tile.waitForExistence(timeout: Self.timeout) else {
                XCTFail("the library never drew a thumbnail")
                startMeasuring()
                stopMeasuring()
                return
            }
            startMeasuring()
            tile.tap()
            let opened = viewer(app).waitForExistence(timeout: Self.timeout)
            stopMeasuring()
            XCTAssertTrue(opened, "the viewer never showed its image")
        }
    }
}
