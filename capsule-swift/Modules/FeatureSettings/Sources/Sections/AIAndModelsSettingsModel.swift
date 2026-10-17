import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - AISlotReport

/// What a model slot is doing, in the vocabulary the screen speaks.
///
/// ``staleExcluded(supersededBy:)`` is the case this type exists for. *AI*
/// says: "A model swap increments `model_version` for that task. Old embeddings
/// are flagged stale and excluded from queries until regenerated from the
/// originals." A search over a stale slot therefore returns **fewer** results,
/// and a UI that showed a shrunken result set without saying why would read as
/// data loss. So exclusion is a reportable state, drawn on screen, and never a
/// silent filter.
public enum AISlotReport: Sendable, Equatable, Hashable {
    case ready(pendingAssetCount: Int)
    case notDownloaded
    case downloading(fractionComplete: Double)
    /// This slot's canonical model changed. Its existing output is excluded
    /// from queries until regenerated — not deleted, and not compared across
    /// versions.
    case staleExcluded(supersededBy: ModelSlot)
    case unsupportedOnThisDevice

    /// The catalog key for the status word.
    public var statusKey: String {
        switch self {
        case .ready: "app.settings.ai.state.ready"
        case .notDownloaded: "app.settings.ai.state.not_downloaded"
        case .downloading: "app.settings.ai.state.downloading"
        case .staleExcluded: "app.settings.ai.state.stale_excluded"
        case .unsupportedOnThisDevice: "app.settings.ai.state.unsupported"
        }
    }

    public var tone: SettingsTone {
        switch self {
        case .ready: .positive
        case .downloading: .neutral
        case .notDownloaded: .neutral
        case .staleExcluded: .caution
        case .unsupportedOnThisDevice: .caution
        }
    }

    /// Whether this slot's output is currently being kept out of queries.
    public var isExcludedFromQueries: Bool {
        if case .staleExcluded = self { return true }
        return false
    }

    /// Read a port status into a report.
    public init(_ status: AIModelStatus) {
        switch status.availability {
        case .ready: self = .ready(pendingAssetCount: status.pendingAssetCount)
        case .notDownloaded: self = .notDownloaded
        case let .downloading(fraction): self = .downloading(fractionComplete: fraction)
        case let .supersededBy(replacement): self = .staleExcluded(supersededBy: replacement)
        case .unsupportedOnThisDevice: self = .unsupportedOnThisDevice
        }
    }
}

// MARK: - AIAndModelsSettingsModel

/// Drives the AI & Models screen: the slots, their versions, their provenance,
/// and the re-index that clears a staleness.
@MainActor
@Observable
public final class AIAndModelsSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var statuses: [AIModelStatus] = []
    public private(set) var isProcessingEnabled = false
    public private(set) var requiresPower = true
    /// The slot a long-running operation is currently on, so only its row shows
    /// progress.
    public private(set) var busySlot: ModelSlot?

    private let intelligence: any AIPort
    private let settings: any SettingsPort
    private let connectivity: SettingsConnectivity

    public init(
        intelligence: any AIPort,
        settings: any SettingsPort,
        connectivity: SettingsConnectivity
    ) {
        self.intelligence = intelligence
        self.settings = settings
        self.connectivity = connectivity
    }

    public func load() async {
        phase = .loading
        do {
            statuses = try await intelligence.modelStatuses()
            isProcessingEnabled = await intelligence.isProcessingEnabled()
            requiresPower = try await settings.settings().aiRequiresPower
            phase = statuses.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// The report for one slot.
    public func report(for status: AIModelStatus) -> AISlotReport {
        AISlotReport(status)
    }

    /// Every slot whose output is currently excluded from queries.
    ///
    /// Non-empty is a normal, temporary state after a model upgrade — the
    /// screen says so rather than letting search quietly return less.
    public var excludedSlots: [ModelSlot] {
        statuses.filter { AISlotReport($0).isExcludedFromQueries }.map(\.slot)
    }

    public var hasStaleExclusions: Bool { !excludedSlots.isEmpty }

    /// Assets still awaiting processing across every runnable slot.
    public var pendingAssetCount: Int {
        statuses.reduce(0) { $0 + $1.pendingAssetCount }
    }

    public func setProcessingEnabled(_ enabled: Bool) async {
        do {
            try await intelligence.setProcessingEnabled(enabled)
            isProcessingEnabled = await intelligence.isProcessingEnabled()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    public func setRequiresPower(_ required: Bool) async {
        do {
            var current = try await settings.settings()
            current.aiRequiresPower = required
            try await settings.update(current)
            requiresPower = required
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Fetch a slot's weights.
    public func download(_ slot: ModelSlot) async {
        await consume(intelligence.downloadModel(slot: slot), slot: slot)
    }

    /// Re-run a slot over the assets whose output went stale.
    ///
    /// This is the *only* way a staleness clears. Nothing regenerates on its
    /// own, because regeneration walks the whole library re-running inference,
    /// and that is a cost the user gets to choose.
    public func regenerate(_ slot: ModelSlot) async {
        await consume(intelligence.regenerate(slot: slot), slot: slot)
    }

    /// Delete a slot's weights and everything derived from it.
    ///
    /// Destructive and confirmed at the call site: output from a slot with no
    /// model is unverifiable, so removing the model has to remove the output —
    /// which is honest, and is also why it cannot be a casual tap.
    public func remove(_ slot: ModelSlot) async {
        do {
            try await intelligence.removeModel(slot: slot)
            statuses = try await intelligence.modelStatuses()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    private func consume(_ stream: AsyncStream<AIModelStatus>, slot: ModelSlot) async {
        busySlot = slot
        defer { busySlot = nil }
        for await update in stream {
            apply(update)
        }
        statuses = await (try? intelligence.modelStatuses()) ?? statuses
    }

    private func apply(_ update: AIModelStatus) {
        guard let index = statuses.firstIndex(where: { $0.slot == update.slot }) else {
            statuses.append(update)
            return
        }
        statuses[index] = update
    }
}
