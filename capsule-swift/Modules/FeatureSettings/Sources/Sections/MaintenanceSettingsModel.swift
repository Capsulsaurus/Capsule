import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - MaintenanceSettingsModel

/// Drives the Maintenance screen: the scheduled integrity jobs, what each one
/// last found, and the two that a user has to ask for by name.
///
/// The rule this model enforces in code rather than in copy comes from
/// *Filesystem — Maintenance*: "Whole-library deduplication is a user-initiated
/// maintenance action or a surfaced suggestion — **never an automatic
/// background deletion**". So ``run(_:userInitiated:)`` refuses to start
/// deduplication unless the caller says a human asked, and
/// ``runScheduledSweep()`` — the path a background wake-up would take — cannot
/// pass that flag. A comment saying "don't call this automatically" would have
/// been a comment; this is a refusal.
@MainActor
@Observable
public final class MaintenanceSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var tasks: [MaintenanceTask] = []
    /// Kinds an automatic sweep asked for and was refused. Surfaced so the
    /// refusal is visible in a test and in a diagnostics report, rather than
    /// being a silently dropped call.
    public private(set) var refusedAutomaticKinds: Set<MaintenanceTaskKind> = []
    /// Kinds started during this screen's lifetime, and how.
    public private(set) var startedKinds: [MaintenanceTaskKind] = []

    private let maintenance: any MaintenancePort
    private let connectivity: SettingsConnectivity

    /// The jobs that may only ever be started by an explicit human action.
    ///
    /// Deduplication merges assets and soft-deletes the losers. It is
    /// reversible, but a user who never asked for it will not know to look in
    /// the trash — which makes an automatic run indistinguishable from data
    /// going missing.
    public static let userInitiatedOnlyKinds: Set<MaintenanceTaskKind> = [
        .intraLibraryDeduplication,
    ]

    public init(maintenance: any MaintenancePort, connectivity: SettingsConnectivity) {
        self.maintenance = maintenance
        self.connectivity = connectivity
    }

    public func load() async {
        phase = .loading
        do {
            tasks = try await maintenance.tasks()
            phase = tasks.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Start a job.
    ///
    /// - Parameter userInitiated: whether a human pressed something. Only a
    ///   view's button passes `true`; every scheduler path leaves it `false`.
    /// - Returns: whether the job actually started.
    @discardableResult
    public func run(_ kind: MaintenanceTaskKind, userInitiated: Bool) async -> Bool {
        guard userInitiated || !Self.userInitiatedOnlyKinds.contains(kind) else {
            refusedAutomaticKinds.insert(kind)
            return false
        }
        startedKinds.append(kind)
        for await update in maintenance.run(kind) {
            apply(update)
        }
        tasks = await (try? maintenance.tasks()) ?? tasks
        return true
    }

    /// What a background wake-up would run.
    ///
    /// Note there is no way to pass `userInitiated: true` from here, and that
    /// is the point: the sweep is a list of kinds, and the gate is applied to
    /// every one of them by the same call the buttons use.
    public func runScheduledSweep() async {
        for task in tasks where !Self.userInitiatedOnlyKinds.contains(task.kind) {
            await run(task.kind, userInitiated: false)
        }
    }

    public func cancel(_ kind: MaintenanceTaskKind) async {
        do {
            try await maintenance.cancel(kind)
            tasks = try await maintenance.tasks()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Whether deduplication has been started at all this session.
    public var didStartDeduplication: Bool {
        startedKinds.contains(.intraLibraryDeduplication)
    }

    /// The duplicate sets the last deduplication pass found and left alone.
    ///
    /// Findings are candidates, never actions: "Resolution is conservative and
    /// never silent. The client presents each duplicate set and lets the user
    /// choose the survivor." A non-zero count means there is a decision waiting,
    /// not that anything was merged.
    public var pendingDuplicateSetCount: Int? {
        guard let task = tasks.first(where: { $0.kind == .intraLibraryDeduplication }),
              case let .completed(_, findingCount) = task.state
        else { return nil }
        return findingCount
    }

    /// One task, by kind.
    public func task(_ kind: MaintenanceTaskKind) -> MaintenanceTask? {
        tasks.first { $0.kind == kind }
    }

    private func apply(_ update: MaintenanceTask) {
        guard let index = tasks.firstIndex(where: { $0.kind == update.kind }) else {
            tasks.append(update)
            return
        }
        tasks[index] = update
    }
}

// MARK: - Presentation

public extension MaintenanceTaskKind {
    /// The catalog key naming this job.
    var titleKey: String { "ios.settings.maintenance.job.\(rawValueKeySuffix)" }

    /// The catalog key explaining what it does and what it costs.
    var detailKey: String { "ios.settings.maintenance.detail.\(rawValueKeySuffix)" }

    private var rawValueKeySuffix: String {
        switch self {
        case .indexReconciliation: "index_reconciliation"
        case .structuralValidation: "structural_validation"
        case .deepContentValidation: "deep_content_validation"
        case .intraLibraryDeduplication: "intra_library_deduplication"
        case .cacheEviction: "cache_eviction"
        case .trashPurge: "trash_purge"
        case .unknown: "unknown"
        }
    }
}

public extension MaintenanceTask.State {
    /// The catalog key for the status word.
    var statusKey: String {
        switch self {
        case .idle: "ios.settings.maintenance.state.idle"
        case .running: "ios.settings.maintenance.state.running"
        case .completed: "ios.settings.maintenance.state.completed"
        case .failed: "ios.settings.maintenance.state.failed"
        case .waitingForConditions: "ios.settings.maintenance.state.waiting"
        }
    }

    var tone: SettingsTone {
        switch self {
        case .idle: .neutral
        case .running: .neutral
        case let .completed(_, findingCount): findingCount == 0 ? .positive : .caution
        case .failed: .critical
        case .waitingForConditions: .neutral
        }
    }

    /// Whether a cancel affordance belongs on the row.
    var isRunning: Bool {
        if case .running = self { return true }
        return false
    }
}
