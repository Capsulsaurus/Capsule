import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - ImportRunItem

/// One row of the running import, built on demand.
///
/// A value produced by ``ImportExecutionModel/item(at:)`` rather than an element
/// of a stored array: a run of a hundred thousand items must not cost a hundred
/// thousand row structs, and the only rows that need to exist are the twenty the
/// list is currently showing.
public struct ImportRunItem: Sendable, Equatable, Identifiable {
    public var index: Int
    public var locator: String
    public var stage: ImportItemStage
    /// The code behind a failure, for the localized message on the row. `nil`
    /// unless ``stage`` is ``ImportItemStage/failed``.
    public var failureCode: ErrorCode?

    public var id: Int { index }

    public init(index: Int, locator: String, stage: ImportItemStage, failureCode: ErrorCode? = nil) {
        self.index = index
        self.locator = locator
        self.stage = stage
        self.failureCode = failureCode
    }

    /// Whether this row offers a retry.
    public var isRetryable: Bool {
        stage == .failed
    }
}

// MARK: - ImportExecutionModel

/// Drives the running-import screen.
///
/// **Nothing per-item is materialised eagerly.** The plan already holds every
/// locator, and the only per-item state a run produces is a stage and, rarely, a
/// failure code — so those live in sparse dictionaries keyed by index and every
/// unmentioned item is ``ImportItemStage/queued`` by definition. A run of a
/// hundred thousand items therefore costs a dictionary of the few hundred that
/// have actually moved, and the list asks for rows one at a time.
///
/// ``apply(_:)`` is the whole state machine, synchronous and public, so a test
/// drives it with a hand-written event sequence instead of racing a stream.
@MainActor
@Observable
public final class ImportExecutionModel {
    /// Where the run stands.
    public enum State: Sendable, Equatable {
        case idle
        case running
        case finished
        case cancelled
    }

    public private(set) var phase: ImportPhase = .ready
    public private(set) var state: State = .idle
    public private(set) var completedCount = 0
    public private(set) var failedCount = 0
    public private(set) var summary: ImportSummary?
    /// The index currently being worked on, for the "now processing" line.
    public private(set) var activeIndex: Int?

    /// Only the items that have moved off ``ImportItemStage/queued``.
    private var stages: [Int: ImportItemStage] = [:]
    private var failures: [Int: ErrorCode] = [:]
    private var retryingIndices: Set<Int> = []

    private let plan: ImportPlan
    private let importing: any ImportPort
    private let connectivity: ImportConnectivity
    private var task: Task<Void, Never>?

    public init(plan: ImportPlan, importing: any ImportPort, connectivity: ImportConnectivity) {
        self.plan = plan
        self.importing = importing
        self.connectivity = connectivity
    }

    public convenience init(plan: ImportPlan, environment: ImportEnvironment) {
        self.init(plan: plan, importing: environment.importing, connectivity: environment.connectivity)
    }

    // MARK: Running

    /// Consume the execution stream to its end.
    public func run() async {
        guard state == .idle else { return }
        state = .running
        let stream = importing.execute(plan)
        let running = Task { @MainActor [weak self] in
            for await event in stream {
                guard let self, !Task.isCancelled else { return }
                apply(event)
            }
        }
        task = running
        await running.value
    }

    /// Ask the port to stop.
    ///
    /// Cancellation is a **stop, not a rollback**: everything already imported
    /// stays imported, and the summary the stream ends with says so. The local
    /// task is torn down only after the port has been told, so the run cannot be
    /// left going with nothing consuming it.
    public func cancel() async {
        try? await importing.cancel(plan.id)
        task?.cancel()
        task = nil
        if state == .running { state = .cancelled }
    }

    /// Fold one event into the screen's state.
    public func apply(_ event: ImportProgressEvent) {
        switch event {
        case .started:
            state = .running
            phase = .ready
        case let .candidateStarted(index, _, _):
            activeIndex = index
            stages[index] = .processing
        case let .candidateStage(index, _, stage):
            stages[index] = stage
        case let .candidateFinished(index, _, outcome):
            record(index: index, outcome: outcome)
        case let .finished(result):
            summary = result
            state = .finished
            activeIndex = nil
        case let .cancelled(result):
            summary = result
            state = .cancelled
            activeIndex = nil
        }
    }

    private func record(index: Int, outcome: ImportOutcome) {
        if case let .failed(code) = outcome {
            stages[index] = .failed
            failures[index] = code
            failedCount += 1
        } else {
            stages[index] = .done
            failures[index] = nil
        }
        completedCount += 1
        if activeIndex == index { activeIndex = nil }
    }

    // MARK: Rows

    /// How many rows the list has.
    public var itemCount: Int { plan.decisions.count }

    /// Every row index, for a lazily-enumerated list.
    public var itemIndices: Range<Int> { 0 ..< itemCount }

    /// One row, built on demand.
    ///
    /// Out-of-range indices return a placeholder rather than trapping: a list
    /// can briefly ask for a row that a re-plan has removed, and crashing on a
    /// scroll would be a poor trade for the invariant.
    public func item(at index: Int) -> ImportRunItem {
        guard plan.decisions.indices.contains(index) else {
            return ImportRunItem(index: index, locator: "", stage: .unknown("out_of_range"))
        }
        return ImportRunItem(
            index: index,
            locator: plan.decisions[index].candidate.locator,
            stage: stages[index] ?? .queued,
            failureCode: failures[index]
        )
    }

    /// Whether a row's retry is in flight.
    public func isRetrying(_ index: Int) -> Bool {
        retryingIndices.contains(index)
    }

    // MARK: Retry

    /// Retry one failed item.
    @discardableResult
    public func retry(_ index: Int) async -> Bool {
        let item = item(at: index)
        guard item.isRetryable else { return false }
        retryingIndices.insert(index)
        defer { retryingIndices.remove(index) }
        do {
            let results = try await importing.retry(plan.id, locators: [item.locator])
            apply(retried: results)
            return failures[index] == nil
        } catch {
            phase = await connectivity.phase(for: error)
            return false
        }
    }

    /// Retry every failed item in one call.
    public func retryAll() async {
        let indices = failures.keys.sorted()
        guard !indices.isEmpty else { return }
        let locators = indices.map { item(at: $0).locator }
        retryingIndices.formUnion(indices)
        defer { retryingIndices.subtract(indices) }
        do {
            let results = try await importing.retry(plan.id, locators: locators)
            apply(retried: results)
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Fold retry results back onto the rows they belong to.
    ///
    /// Keyed by locator because that is what the port takes and returns; the
    /// index is this screen's private business and the port has no reason to
    /// know it.
    private func apply(retried results: [ImportResult]) {
        var byLocator: [String: ImportOutcome] = [:]
        for result in results {
            byLocator[result.locator] = result.outcome
        }
        for index in failures.keys {
            guard let outcome = byLocator[item(at: index).locator] else { continue }
            if case let .failed(code) = outcome {
                failures[index] = code
            } else {
                stages[index] = .done
                failures[index] = nil
                failedCount = max(0, failedCount - 1)
            }
        }
    }

    // MARK: Derived

    /// Completed fraction, 0…1.
    public var fraction: Double {
        guard itemCount > 0 else { return 0 }
        return min(1, Double(completedCount) / Double(itemCount))
    }

    /// Whether the cancel affordance belongs on screen.
    public var isCancellable: Bool { state == .running }

    /// Whether anything can be retried.
    public var hasRetryableFailures: Bool { failedCount > 0 }
}
