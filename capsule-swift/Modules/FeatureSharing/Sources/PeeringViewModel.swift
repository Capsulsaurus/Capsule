import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - PeeringViewModel

/// LAN transfers between this user's own devices (*Peering*).
///
/// The load-bearing UI rule here is that **finding no peer is not a failure**.
/// "If no peer is found, peering simply does nothing and the device falls back
/// to server sync. Nothing depends on it succeeding." A vanished peer and a
/// never-present one are indistinguishable by design, so this model has no
/// error state for an empty peer list, offers no retry for it, and never
/// reports it as a problem.
///
/// Peering is also not sharing: every device here is one of the *same user's*,
/// confirmed against the account's device directory. Discovery alone proves
/// nothing — mDNS advertisements are opaque and rotate, so a name on the network
/// is not an identity.
@MainActor
@Observable
public final class PeeringViewModel {
    public private(set) var isEnabled = false
    public private(set) var peers: [LocalPeer] = []
    public private(set) var transfers: [PeeringTransfer] = []
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?

    private let peering: any PeeringPort
    private let connectivity: SharingConnectivity
    // Not observed and not isolated: it is a cancellation handle, never
    // rendered, and `deinit` must be able to cancel it without hopping actors.
    @ObservationIgnored
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        peering: any PeeringPort,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        self.peering = peering
        self.connectivity = connectivity
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived state

    /// Devices that may actually be transferred with. Only a device already in
    /// the account's directory reaches `paired`.
    public var pairedPeers: [LocalPeer] {
        peers.filter(\.permitsTransfer)
    }

    /// Seen on the network but not confirmed as this user's, or confirmed and
    /// since revoked. Shown, because "we can see it and will not talk to it" is
    /// a state worth understanding, not hidden as if it did not exist.
    public var unpairedPeers: [LocalPeer] {
        peers.filter { !$0.permitsTransfer }
    }

    /// Whether the screen is in its ordinary quiet state: peering on, nothing
    /// nearby. Distinct from an error and rendered as such.
    public var isIdleWithNoPeers: Bool {
        isEnabled && peers.isEmpty
    }

    // MARK: Actions

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        connection = await connectivity.probe()
        isEnabled = await peering.isEnabled()
        guard isEnabled else {
            peers = []
            transfers = []
            phase = .ready
            return
        }
        do {
            peers = try await peering.discoveredPeers()
            transfers = try await peering.activeTransfers()
            // Deliberately `.empty`, never `.failed`: an empty LAN is the
            // expected outcome most of the time.
            phase = peers.isEmpty ? .empty : .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Turn peering on or off on this device.
    public func setEnabled(_ enabled: Bool) async {
        do {
            try await peering.setEnabled(enabled)
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
        await reload()
    }

    /// Pull originals from a paired peer.
    ///
    /// Pull-only by design: the device that is behind initiates, and applies
    /// the result only after verifying it. A blob from a peer is checked against
    /// its content address exactly as a server blob is.
    public func requestOriginals(for assetIDs: [AssetID], from peer: DeviceID) async {
        do {
            try await peering.requestOriginals(for: assetIDs, from: peer)
            await reload()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    private func observeChanges() {
        observation?.cancel()
        let port = peering
        observation = Task { [weak self] in
            for await _ in port.changes() {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
