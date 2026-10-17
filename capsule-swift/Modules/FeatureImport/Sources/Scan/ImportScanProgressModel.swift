import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - ImportScanProgressModel

/// Drives the scan screen: how far the enumeration has got, what it has found,
/// and whether it can still be stopped.
///
/// ``apply(_:)`` is the whole state machine and is deliberately synchronous and
/// public: a test drives it with a hand-written event sequence and asserts the
/// result, rather than racing a stream and sleeping until it settles. The async
/// ``start()`` does nothing but feed it.
@MainActor
@Observable
public final class ImportScanProgressModel {
    /// Where the scan stands.
    public enum State: Sendable, Equatable {
        case idle
        case scanning
        case finished
        case cancelled
    }

    public private(set) var phase: ImportPhase = .loading
    public private(set) var state: State = .idle
    public private(set) var progress: ImportScanProgress
    /// The completed scan, once there is one.
    public private(set) var scan: ImportScan?

    private let scope: ImportScope
    private let importing: any ImportPort
    private let connectivity: ImportConnectivity
    private var task: Task<Void, Never>?

    public init(
        scope: ImportScope,
        importing: any ImportPort,
        connectivity: ImportConnectivity
    ) {
        self.scope = scope
        self.importing = importing
        self.connectivity = connectivity
        progress = ImportScanProgress(itemsFound: 0)
    }

    public convenience init(scope: ImportScope, environment: ImportEnvironment) {
        self.init(scope: scope, importing: environment.importing, connectivity: environment.connectivity)
    }

    /// The source being read.
    public var source: ImportScope { scope }

    /// Consume the scan stream until it ends or the task is cancelled.
    ///
    /// Held as a `Task` rather than run inline so ``cancel()`` has something to
    /// cancel: a scan writes nothing, so tearing down the consumer *is* the
    /// stop, and there is no second cancellation path to keep consistent with
    /// this one.
    public func start() async {
        guard state == .idle else { return }
        state = .scanning
        phase = .ready
        let stream = importing.scanStream(scope)
        let running = Task { @MainActor [weak self] in
            for await event in stream {
                guard let self, !Task.isCancelled else { return }
                apply(event)
            }
        }
        task = running
        await running.value
    }

    /// Stop the scan. Nothing was written, so there is nothing to undo.
    public func cancel() {
        task?.cancel()
        task = nil
        guard state == .scanning else { return }
        apply(.cancelled(itemsFound: progress.itemsFound))
    }

    /// Fold one event into the screen's state.
    public func apply(_ event: ImportScanEvent) {
        switch event {
        case let .started(expectedTotal):
            state = .scanning
            phase = .ready
            progress = ImportScanProgress(itemsFound: 0, expectedTotal: expectedTotal)
        case let .progress(update):
            // The declared total is kept from `.started` when a tick omits it:
            // a bar that fell back to indeterminate mid-scan would read as a
            // fault rather than as missing information.
            progress = ImportScanProgress(
                itemsFound: update.itemsFound,
                bytesFound: update.bytesFound,
                currentLocator: update.currentLocator,
                expectedTotal: update.expectedTotal ?? progress.expectedTotal
            )
        case let .finished(result):
            scan = result
            state = .finished
            progress = ImportScanProgress(
                itemsFound: result.candidates.count,
                bytesFound: result.totalKnownBytes,
                expectedTotal: result.candidates.count
            )
            phase = result.candidates.isEmpty ? .empty : .ready
        case let .cancelled(itemsFound):
            state = .cancelled
            progress = ImportScanProgress(
                itemsFound: itemsFound,
                bytesFound: progress.bytesFound,
                expectedTotal: progress.expectedTotal
            )
        }
    }

    /// Record a thrown error the stream could not carry.
    public func fail(_ error: any Error) async {
        phase = await connectivity.phase(for: error)
    }

    /// Whether the cancel affordance belongs on screen.
    public var isCancellable: Bool {
        state == .scanning
    }

    /// Locators the scanner could not read at all — a permissions problem, not a
    /// format problem, and surfaced rather than silently dropped.
    public var unreadableLocators: [String] {
        scan?.unreadableLocators ?? []
    }

    /// Whether the user can move on to the plan.
    public var canContinue: Bool {
        guard let scan else { return false }
        return !scan.candidates.isEmpty
    }
}
