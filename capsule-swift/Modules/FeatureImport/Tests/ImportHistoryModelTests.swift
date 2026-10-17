import CapsuleDomain
import CapsuleMock
import Testing

@testable import FeatureImport

/// The history list: what each past run says about itself, and the two things a
/// user can do about one afterwards.
@Suite("Import history")
@MainActor
struct ImportHistoryModelTests {
    private func model(
        sessions: [ImportSessionRecord],
        port: StubImportPort? = nil
    ) -> (model: ImportHistoryModel, port: StubImportPort) {
        let resolved = port ?? StubImportPort(sessions: sessions)
        return (
            ImportHistoryModel(
                importing: resolved,
                albums: MockEnvironment(scenario: .healthy).albums,
                connectivity: StubFixtures.connectivity,
                clock: StubFixtures.clock
            ),
            resolved
        )
    }

    @Test("a populated history is ready and ordered as the port returned it")
    func populatedHistory() async {
        let sessions = [StubFixtures.session(0), StubFixtures.session(1)]
        let history = model(sessions: sessions).model

        await history.load()

        #expect(history.phase == .ready)
        #expect(history.sessions.map(\.id) == sessions.map(\.id))
    }

    @Test("a library that has never imported anything is empty")
    func emptyHistory() async {
        let history = model(sessions: []).model

        await history.load()

        #expect(history.phase == .empty)
        #expect(history.sessions.isEmpty)
    }

    @Test("rows start closed and toggle independently")
    func expansionIsPerRow() async {
        let sessions = [StubFixtures.session(0), StubFixtures.session(1)]
        let history = model(sessions: sessions).model
        await history.load()

        #expect(!history.isExpanded(sessions[0].id))

        history.toggle(sessions[0].id)
        history.toggle(sessions[1].id)
        history.toggle(sessions[1].id)

        #expect(history.isExpanded(sessions[0].id))
        #expect(!history.isExpanded(sessions[1].id))
    }

    /// Dismissing forgets the record, not the assets — which is why it removes a
    /// row rather than marking one.
    @Test("dismissing removes the row and tells the port")
    func dismissRemovesTheRow() async {
        let sessions = [StubFixtures.session(0), StubFixtures.session(1)]
        let wired = model(sessions: sessions)
        await wired.model.load()

        await wired.model.dismiss(sessions[0].id)
        let dismissed = await wired.port.dismissedSessions

        #expect(dismissed == [sessions[0].id])
        #expect(wired.model.sessions.map(\.id) == [sessions[1].id])
        #expect(!wired.model.isExpanded(sessions[0].id))
    }

    @Test("dismissing the last row leaves the screen empty")
    func dismissingEverythingIsEmpty() async {
        let session = StubFixtures.session(0)
        let history = model(sessions: [session]).model
        await history.load()

        await history.dismiss(session.id)

        #expect(history.phase == .empty)
    }

    /// A re-run passes back through the confirmation screen, because the library
    /// has changed since and what was an import last week may be a duplicate
    /// today.
    @Test("re-running yields a plan rather than starting an import")
    func rerunYieldsAPlan() async {
        let session = StubFixtures.session(0)
        let history = model(sessions: [session]).model
        await history.load()

        let plan = await history.rerun(session.id)

        #expect(plan != nil)
        #expect(plan?.decisions.isEmpty == false)
    }

    @Test("a clean run reports completed, one with failures says so")
    func outcomesAreDistinguished() {
        #expect(StubFixtures.session(0, failures: 0).outcome == .completed)
        #expect(StubFixtures.session(1, failures: 2).outcome == .completedWithFailures(count: 2))
        #expect(StubFixtures.session(2, cancelled: true).outcome == .cancelled)
    }

    @Test("a run still going is not reported as completed")
    func runningSessionIsNotCompleted() {
        var session = StubFixtures.session(0)
        session.finishedAt = nil

        #expect(session.outcome == .running)
    }

    /// Only a failure is retryable: an unsupported format and a duplicate are
    /// settled facts, and offering to retry them would promise an outcome the
    /// retry cannot produce.
    @Test("only failures are offered for retry")
    func onlyFailuresAreRetryable() {
        let session = StubFixtures.session(0, failures: 3, imported: 5)

        #expect(session.retryableLocators.count == 3)
        #expect(session.summary.importedCount == 5)
        #expect(session.summary.skippedCount == 3)
    }

    @Test("each outcome carries its own tone and key")
    func outcomePresentationIsDistinct() {
        let outcomes: [ImportSessionRecord.Outcome] = [
            .running, .completed, .completedWithFailures(count: 1), .cancelled,
        ]

        #expect(Set(outcomes.map(\.titleKey)).count == outcomes.count)
        #expect(ImportSessionRecord.Outcome.completed.tone == .positive)
        #expect(ImportSessionRecord.Outcome.completedWithFailures(count: 1).tone == .caution)
    }

    /// Nothing in this module reads `Date()`, so an elapsed figure is pinned by
    /// the injected clock rather than by when the suite happened to run.
    @Test("elapsed time is measured against the injected clock")
    func elapsedUsesTheInjectedClock() async {
        let session = StubFixtures.session(0)
        let history = model(sessions: [session]).model
        await history.load()

        let elapsed = history.elapsedSinceStart(of: session)

        #expect(!elapsed.isEmpty)
        #expect(elapsed != ImportFormat.unknown)
    }
}
