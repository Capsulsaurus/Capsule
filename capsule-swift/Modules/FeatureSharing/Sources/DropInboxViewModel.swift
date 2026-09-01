import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - DropInboxViewModel

/// The guest-upload inbox — drops "awaiting your review"
/// (*Web Upload — Drop and Adoption Lifecycle*).
///
/// One of the eight quarantine surfaces, and it behaves like one: nothing is
/// applied without an explicit human decision. A drop is unauthenticated content
/// from a stranger, and nothing about it is trustworthy until a trusted client
/// decapsulates the file key and the AEAD tags verify — so every field on a card
/// is presented as a claim, never as metadata.
@MainActor
@Observable
public final class DropInboxViewModel {
    public private(set) var drops: [PendingDrop] = []
    public private(set) var totalCount: Int?
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?
    /// The drop shown in the detail pane. A plain selection, so the wide layout
    /// and the narrow stack drive the same state.
    public var selection: DropID?

    private let dropPort: any DropPort
    private let connectivity: SharingConnectivity
    private let pageSize: Int
    // Not observed and not isolated: it is a cancellation handle, never
    // rendered, and `deinit` must be able to cancel it without hopping actors.
    @ObservationIgnored
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        drops dropPort: any DropPort,
        connectivity: SharingConnectivity = SharingConnectivity(),
        pageSize: Int = 50
    ) {
        self.dropPort = dropPort
        self.connectivity = connectivity
        self.pageSize = pageSize
    }

    deinit {
        observation?.cancel()
    }

    /// The selected drop, resolved against the loaded window.
    public var selected: PendingDrop? {
        guard let selection else { return nil }
        return drops.first { $0.id == selection }
    }

    /// Load the first window and begin observing. Call once, on appear.
    public func load() async {
        await reload()
        observeChanges()
    }

    /// Re-read the inbox. Also the completion step of an adopt or a discard, so
    /// the card leaves the list only once the port says it has.
    public func reload() async {
        connection = await connectivity.probe()
        do {
            let page = try await dropPort.pendingDrops(offset: 0, limit: pageSize)
            drops = page.items
            totalCount = page.totalCount
            phase = page.items.isEmpty ? .empty : .ready
            if let selection, !page.items.contains(where: { $0.id == selection }) {
                self.selection = nil
            }
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    private func observeChanges() {
        observation?.cancel()
        let port = dropPort
        observation = Task { [weak self] in
            for await _ in port.changes() {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
