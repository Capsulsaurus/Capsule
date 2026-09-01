import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - ImportPlanCategory

/// The three buckets the plan divides into, and the one the user has opened.
///
/// A closed enum rather than three booleans: exactly one list is expanded at a
/// time, and three booleans admit the state where all three are, which on a
/// fifteen-hundred-item plan is three lists nobody can read.
public enum ImportPlanCategory: Sendable, Equatable, Hashable, CaseIterable, Identifiable {
    case add
    case skip
    case conflicts

    public var id: Self { self }

    /// The catalog key for the tile's label.
    public var titleKey: String {
        switch self {
        case .add: "app.import.plan.tile.add"
        case .skip: "app.import.plan.tile.skip"
        case .conflicts: "app.import.plan.tile.conflicts"
        }
    }

    /// The catalog key for the expanded list's header.
    public var listHeaderKey: String {
        switch self {
        case .add: "app.import.plan.list.add.header"
        case .skip: "app.import.plan.list.skip.header"
        case .conflicts: "app.import.plan.list.conflicts.header"
        }
    }

    public var symbol: String {
        switch self {
        case .add: "plus.circle"
        case .skip: "minus.circle"
        case .conflicts: "exclamationmark.triangle"
        }
    }
}

// MARK: - ImportPlanConfirmModel

/// Drives the confirmation screen — the one place a bulk, partly irreversible
/// operation is consented to.
///
/// Three things this model refuses to leave implicit, because each of them is a
/// way a user ends up surprised by their own library:
///
/// - **The destination is never shown without the rule that chose it.** The
///   resolution ladder is five rungs deep and a scope override fires silently;
///   "why did those land there" has to be answerable on this screen rather than
///   reconstructed later.
/// - **Free space is assessed against the *plan*, not the library.** A run that
///   fits only barely has a remedy that is not deletion — streaming — and one
///   that does not fit at all must say how much to free.
/// - **Conflicts are answered, not defaulted past.** Confirm is gated on every
///   conflict carrying a resolution its kind admits.
@MainActor
@Observable
public final class ImportPlanConfirmModel {
    public private(set) var phase: ImportPhase = .loading
    public private(set) var plan: ImportPlan?
    public private(set) var breakdown: LocalStorageBreakdown = .init()
    /// The destination album's name, or `nil` for the nameless default album.
    public private(set) var destinationName: String?
    /// Which stat tile's list is open.
    public var expanded: ImportPlanCategory?

    private let scan: ImportScan
    private let importing: any ImportPort
    private let storage: any StoragePort
    private let albums: any AlbumPort
    private let connectivity: ImportConnectivity
    private var mode: ImportMode
    private var uploadPolicy: UploadPolicy
    private var streaming: Bool

    public init(
        scan: ImportScan,
        importing: any ImportPort,
        storage: any StoragePort,
        albums: any AlbumPort,
        connectivity: ImportConnectivity,
        mode: ImportMode = .copy,
        uploadPolicy: UploadPolicy = .full,
        streaming: Bool = false
    ) {
        self.scan = scan
        self.importing = importing
        self.storage = storage
        self.albums = albums
        self.connectivity = connectivity
        self.mode = mode
        self.uploadPolicy = uploadPolicy
        self.streaming = streaming
    }

    public convenience init(
        scan: ImportScan,
        environment: ImportEnvironment,
        mode: ImportMode = .copy,
        uploadPolicy: UploadPolicy = .full,
        streaming: Bool = false
    ) {
        self.init(
            scan: scan,
            importing: environment.importing,
            storage: environment.storage,
            albums: environment.albums,
            connectivity: environment.connectivity,
            mode: mode,
            uploadPolicy: uploadPolicy,
            streaming: streaming
        )
    }

    // MARK: Loading

    /// Plan the scan and read the disk.
    ///
    /// The breakdown is read on a best-effort basis: a plan whose free-space
    /// figure is missing still confirms, because refusing to import over an
    /// unreadable disk metric would block the operation on the least important
    /// of its inputs.
    public func load() async {
        phase = .loading
        do {
            let planned = try await importing.plan(
                scan,
                destination: nil,
                mode: mode,
                uploadPolicy: uploadPolicy,
                streaming: streaming
            )
            plan = planned
            breakdown = await (try? storage.localBreakdown()) ?? LocalStorageBreakdown()
            destinationName = try? await albums.containerAlbum(planned.destinationAlbumID)?.name
            phase = planned.decisions.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    // MARK: Derived figures

    /// Items the scan produced, whatever the planner decided about them.
    public var totalCount: Int {
        plan?.decisions.count ?? 0
    }

    /// Bytes the run will write.
    public var totalBytes: UInt64 {
        plan?.estimatedByteSize ?? 0
    }

    /// The count behind one stat tile.
    public func count(for category: ImportPlanCategory) -> Int {
        guard let plan else { return 0 }
        switch category {
        case .add: return plan.importCount
        case .skip: return plan.skipCount
        case .conflicts: return plan.conflicts.count
        }
    }

    /// The decisions behind the Add or Skip tile.
    public func decisions(for category: ImportPlanCategory) -> [ImportDecision] {
        guard let plan else { return [] }
        switch category {
        case .add: return plan.decisions.filter(\.isImporting)
        case .skip: return plan.decisions.filter { !$0.isImporting }
        case .conflicts: return []
        }
    }

    /// The open conflicts.
    public var conflicts: [ImportConflict] {
        plan?.conflicts ?? []
    }

    /// Whether the device has room, and what to do if it barely does.
    public var outlook: ImportSpaceOutlook {
        ImportSpaceOutlook.assess(
            requiredBytes: totalBytes,
            availableBytes: breakdown.availableDiskBytes
        )
    }

    /// Which rule resolved the destination. Never rendered without it.
    public var destinationRule: ImportPlan.DestinationRule? {
        plan?.destinationRule
    }

    /// Whether this run will release local bytes as it goes.
    public var isStreaming: Bool { streaming }

    /// Whether the run will delete the user's source files.
    public var releasesSource: Bool {
        plan?.mode.releasesSource ?? false
    }

    // MARK: Decisions

    /// Answer one conflict.
    public func resolve(_ candidateID: String, as resolution: ImportConflictResolution) {
        guard let current = plan else { return }
        plan = current.resolving(candidateID, as: resolution)
    }

    /// Turn streaming on or off, re-planning against the port.
    ///
    /// Re-planned rather than toggled on the local value because the planner
    /// **rejects** streaming combined with a staged upload policy outright, and
    /// a client that flipped the flag locally would be presenting a plan the
    /// executor will refuse.
    public func setStreaming(_ enabled: Bool) async {
        guard enabled != streaming else { return }
        streaming = enabled
        await load()
    }

    /// Switch between copying and moving. A move deletes the source files after
    /// a durable verdict, so it is re-planned rather than applied to a plan the
    /// user already read.
    public func setMode(_ newMode: ImportMode) async {
        guard newMode != mode else { return }
        mode = newMode
        await load()
    }

    /// Whether the confirm action may fire.
    ///
    /// Gated on the space verdict and on every conflict being answered — never
    /// on there being no conflicts, which would make a conflict a dead end
    /// instead of a decision.
    public var canConfirm: Bool {
        guard let plan, phase.isReady else { return false }
        guard plan.importCount > 0 else { return false }
        guard !plan.violatesStagedStreamingExclusion else { return false }
        return plan.conflictsAreResolved && outlook.permitsImport
    }

    /// The plan the user consented to, or `nil` if they may not yet.
    public func confirm() -> ImportPlan? {
        guard canConfirm else { return nil }
        return plan
    }
}
