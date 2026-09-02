import CapsuleDomain
import Testing

@testable import FeatureImport

/// The scan screen's state machine, driven by hand-written event sequences.
///
/// Driven through ``ImportScanProgressModel/apply(_:)`` rather than by consuming
/// a live stream and waiting: a test that slept until a stream settled would be
/// asserting on the scheduler as much as on the model.
@Suite("Import scan progress")
@MainActor
struct ImportScanProgressModelTests {
    private func model(scope: ImportScope = StubFixtures.cameraRollScope) -> ImportScanProgressModel {
        ImportScanProgressModel(
            scope: scope,
            importing: StubImportPort(scan: StubFixtures.scan(candidateCount: 3)),
            connectivity: StubFixtures.connectivity
        )
    }

    @Test("a declared total makes the indicator determinate")
    func determinateWhenTotalDeclared() {
        let scan = model()

        scan.apply(.started(expectedTotal: 120))
        scan.apply(.progress(ImportScanProgress(itemsFound: 30, bytesFound: 1000, expectedTotal: 120)))

        #expect(scan.progress.isDeterminate)
        #expect(scan.progress.fraction == 0.25)
    }

    @Test("no declared total leaves the indicator indeterminate")
    func indeterminateWhenTotalUnknown() {
        let scan = model()

        scan.apply(.started(expectedTotal: nil))
        scan.apply(.progress(ImportScanProgress(itemsFound: 30, bytesFound: 1000)))

        #expect(!scan.progress.isDeterminate)
        #expect(scan.progress.fraction == nil)
        #expect(scan.progress.itemsFound == 30)
    }

    /// A bar that fell back to indeterminate part-way through would read as a
    /// fault rather than as missing information.
    @Test("a tick that omits the total keeps the one from the start event")
    func totalSurvivesATickThatOmitsIt() {
        let scan = model()

        scan.apply(.started(expectedTotal: 90))
        scan.apply(.progress(ImportScanProgress(itemsFound: 9, bytesFound: 500)))

        #expect(scan.progress.expectedTotal == 90)
        #expect(scan.progress.fraction == 0.1)
    }

    @Test("finishing publishes the scan and unlocks the next step")
    func finishingPublishesTheScan() {
        let scan = model()
        let result = StubFixtures.scan(candidateCount: 4, byteSize: 2000000)

        scan.apply(.started(expectedTotal: 4))
        scan.apply(.finished(result))

        #expect(scan.state == .finished)
        #expect(scan.scan == result)
        #expect(scan.canContinue)
        #expect(scan.progress.itemsFound == 4)
        #expect(scan.progress.bytesFound == 8000000)
        #expect(!scan.isCancellable)
    }

    @Test("a scan that found nothing is empty rather than ready")
    func emptyScanIsEmpty() {
        let scan = model()

        scan.apply(.started(expectedTotal: 0))
        scan.apply(.finished(ImportScan(scope: StubFixtures.cameraRollScope, candidates: [])))

        #expect(scan.phase == .empty)
        #expect(!scan.canContinue)
    }

    /// A scan writes nothing, so cancelling keeps the tally and produces no
    /// rollback to explain.
    @Test("cancelling keeps the count it had reached")
    func cancellingKeepsTheCount() {
        let scan = model()

        scan.apply(.started(expectedTotal: nil))
        scan.apply(.progress(ImportScanProgress(itemsFound: 17, bytesFound: 900)))
        scan.cancel()

        #expect(scan.state == .cancelled)
        #expect(scan.progress.itemsFound == 17)
        #expect(!scan.canContinue)
        #expect(!scan.isCancellable)
    }

    @Test("cancelling after the scan finished changes nothing")
    func cancelAfterFinishIsInert() {
        let scan = model()
        scan.apply(.finished(StubFixtures.scan(candidateCount: 2)))

        scan.cancel()

        #expect(scan.state == .finished)
        #expect(scan.canContinue)
    }

    /// A permissions problem is a different thing from an unsupported format,
    /// and is surfaced rather than silently dropped.
    @Test("unreadable locators reach the screen")
    func unreadableLocatorsAreSurfaced() {
        let scan = model()
        let result = ImportScan(
            scope: StubFixtures.cameraRollScope,
            candidates: [StubFixtures.candidate(0)],
            unreadableLocators: ["photokit://camera-roll/locked/IMG_1.HEIC"]
        )

        scan.apply(.finished(result))

        #expect(scan.unreadableLocators.count == 1)
    }

    @Test("consuming the port's stream lands on a finished scan")
    func streamDrivesTheModel() async {
        let scan = model()

        await scan.start()

        #expect(scan.state == .finished)
        #expect(scan.scan?.candidates.count == 3)
    }
}
