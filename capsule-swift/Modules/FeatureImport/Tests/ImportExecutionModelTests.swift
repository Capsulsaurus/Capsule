import CapsuleDomain
import Testing

@testable import FeatureImport

/// The running-import screen: the stage ladder, the sparse row state that keeps
/// a hundred-thousand-item run affordable, and the retry path.
@Suite("Import execution")
@MainActor
struct ImportExecutionModelTests {
    private func model(
        itemCount: Int = 5,
        retryResults: [ImportResult] = [],
        events: [ImportProgressEvent] = []
    ) -> ImportExecutionModel {
        ImportExecutionModel(
            plan: PreviewPlans.plan(itemCount: itemCount),
            importing: StubImportPort(retryResults: retryResults, events: events),
            connectivity: StubFixtures.connectivity
        )
    }

    private func locator(_ ordinal: Int) -> String {
        "photokit://camera-roll/IMG_\(4000 + ordinal).HEIC"
    }

    /// The whole point of the sparse dictionary: an untouched row is queued by
    /// definition, so a run does not pay for rows nothing has happened to.
    @Test("an untouched row is queued without anything having been stored for it")
    func untouchedRowsAreQueued() {
        let run = model(itemCount: 50000)

        #expect(run.itemCount == 50000)
        #expect(run.item(at: 49999).stage == .queued)
        #expect(run.item(at: 0).stage == .queued)
        #expect(run.completedCount == 0)
    }

    @Test("an out-of-range row is a placeholder rather than a trap")
    func outOfRangeIsSafe() {
        let run = model(itemCount: 3)

        #expect(run.item(at: 99).stage == .unknown("out_of_range"))
    }

    @Test("an item walks queued → processing → encrypting → uploading → done")
    func stageLadderIsFollowed() {
        let run = model()

        run.apply(.started(importID: ImportID("preview-import"), totalCandidates: 5))
        run.apply(.candidateStarted(index: 0, total: 5, locator: locator(0)))
        #expect(run.item(at: 0).stage == .processing)

        run.apply(.candidateStage(index: 0, locator: locator(0), stage: .encrypting))
        #expect(run.item(at: 0).stage == .encrypting)

        run.apply(.candidateStage(index: 0, locator: locator(0), stage: .uploading))
        #expect(run.item(at: 0).stage == .uploading)

        run.apply(.candidateFinished(index: 0, locator: locator(0), outcome: .imported(assetID: "a", derivativesDeferred: false)))
        #expect(run.item(at: 0).stage == .done)
        #expect(run.completedCount == 1)
        #expect(run.failedCount == 0)
    }

    /// A failure carries its code so the row can show a localized message rather
    /// than the English diagnostic detail.
    @Test("a failure records a retryable row with its error code")
    func failureIsRetryable() {
        let run = model()

        run.apply(.candidateFinished(index: 2, locator: locator(2), outcome: .failed(.uploadChecksumMismatch)))
        let item = run.item(at: 2)

        #expect(item.stage == .failed)
        #expect(item.failureCode == .uploadChecksumMismatch)
        #expect(item.isRetryable)
        #expect(run.failedCount == 1)
        #expect(run.hasRetryableFailures)
    }

    @Test("a skipped candidate is complete, not failed")
    func skipIsNotFailure() {
        let run = model()

        run.apply(.candidateFinished(index: 1, locator: locator(1), outcome: .duplicateSkipped(existingAssetID: "x")))

        #expect(run.item(at: 1).stage == .done)
        #expect(run.failedCount == 0)
        #expect(run.completedCount == 1)
    }

    @Test("a successful retry clears the failure")
    func retrySucceeds() async {
        let run = model(retryResults: [
            ImportResult(locator: locator(3), outcome: .imported(assetID: "asset", derivativesDeferred: false)),
        ])
        run.apply(.candidateFinished(index: 3, locator: locator(3), outcome: .failed(.blobPendingUpload)))

        let recovered = await run.retry(3)

        #expect(recovered)
        #expect(run.item(at: 3).stage == .done)
        #expect(run.item(at: 3).failureCode == nil)
        #expect(run.failedCount == 0)
        #expect(!run.hasRetryableFailures)
    }

    @Test("a retry that fails again keeps the row retryable, with the new code")
    func retryFailsAgain() async {
        let run = model(retryResults: [
            ImportResult(locator: locator(1), outcome: .failed(.uploadStorageInconsistent)),
        ])
        run.apply(.candidateFinished(index: 1, locator: locator(1), outcome: .failed(.blobPendingUpload)))

        let recovered = await run.retry(1)

        #expect(!recovered)
        #expect(run.item(at: 1).failureCode == .uploadStorageInconsistent)
        #expect(run.failedCount == 1)
    }

    /// No amount of retrying gives this build a codec it does not have, so a row
    /// that did not fail must not offer one.
    @Test("a row that did not fail is not retried")
    func nonFailedRowsAreNotRetried() async {
        let run = model()
        run.apply(.candidateFinished(index: 0, locator: locator(0), outcome: .unsupported))

        let retried = await run.retry(0)

        #expect(!retried)
    }

    @Test("retry-all sends every failed locator in one call")
    func retryAllBatches() async {
        let results = [
            ImportResult(locator: locator(0), outcome: .imported(assetID: "a", derivativesDeferred: false)),
            ImportResult(locator: locator(4), outcome: .imported(assetID: "b", derivativesDeferred: false)),
        ]
        let port = StubImportPort(retryResults: results)
        let run = ImportExecutionModel(
            plan: PreviewPlans.plan(itemCount: 5),
            importing: port,
            connectivity: StubFixtures.connectivity
        )
        run.apply(.candidateFinished(index: 0, locator: locator(0), outcome: .failed(.blobPendingUpload)))
        run.apply(.candidateFinished(index: 4, locator: locator(4), outcome: .failed(.blobPendingUpload)))

        await run.retryAll()
        let asked = await port.retriedLocators

        #expect(Set(asked) == Set([locator(0), locator(4)]))
        #expect(run.failedCount == 0)
    }

    /// Cancellation is a stop, not a rollback: the summary carries what already
    /// landed.
    @Test("cancelling keeps what already imported")
    func cancellationKeepsProgress() async {
        let port = StubImportPort()
        let run = ImportExecutionModel(
            plan: PreviewPlans.plan(itemCount: 5),
            importing: port,
            connectivity: StubFixtures.connectivity
        )
        run.apply(.started(importID: ImportID("preview-import"), totalCandidates: 5))
        run.apply(.candidateFinished(index: 0, locator: locator(0), outcome: .imported(assetID: "a", derivativesDeferred: false)))

        await run.cancel()
        let cancelled = await port.didCancel

        #expect(cancelled)
        #expect(run.state == .cancelled)
        #expect(run.completedCount == 1)
        #expect(run.item(at: 0).stage == .done)
    }

    @Test("the progress fraction tracks completions")
    func fractionTracksCompletions() {
        let run = model(itemCount: 4)

        run.apply(.candidateFinished(index: 0, locator: locator(0), outcome: .unsupported))
        run.apply(.candidateFinished(index: 1, locator: locator(1), outcome: .unsupported))

        #expect(run.fraction == 0.5)
    }

    @Test("consuming a whole stream ends on the summary")
    func streamRunsToCompletion() async {
        let summary = ImportSummary(
            id: ImportID("preview-import"),
            results: [ImportResult(locator: locator(0), outcome: .imported(assetID: "a", derivativesDeferred: false))]
        )
        let run = model(events: [
            .started(importID: ImportID("preview-import"), totalCandidates: 5),
            .candidateStarted(index: 0, total: 5, locator: locator(0)),
            .candidateStage(index: 0, locator: locator(0), stage: .uploading),
            .candidateFinished(index: 0, locator: locator(0), outcome: .imported(assetID: "a", derivativesDeferred: false)),
            .finished(summary: summary),
        ])

        await run.run()

        #expect(run.state == .finished)
        #expect(run.summary == summary)
        #expect(run.completedCount == 1)
        #expect(!run.isCancellable)
    }
}
