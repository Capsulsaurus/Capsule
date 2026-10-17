import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - DropPreviewState

/// How much of a drop can safely be shown.
///
/// Adopted bytes are **external-origin**: no device the user trusts authored a
/// drop's plaintext, so every client — including the adopter, on preview —
/// decodes them only in the sandboxed decoder (*Web Upload — Trust Boundary*).
/// A decoder defect therefore crashes a sandbox rather than the photo library,
/// and ``unavailable`` is a normal outcome the review screen must render, with
/// discard still reachable.
public enum DropPreviewState: Sendable, Equatable, Hashable {
    /// The sandboxed decode has not been asked for yet.
    case pending
    /// The sandboxed decode is running.
    case decoding
    /// The sandbox produced no image. The user can still discard it unseen.
    case unavailable
}

// MARK: - DropDetailViewModel

/// Review one drop and decide (*Web Upload — Drop and Adoption Lifecycle*).
///
/// Adoption is an **atomic promotion**: the asset row, its provenance, and the
/// inbox-row deletion land in one transaction, so there is no half-adopted
/// state to render. Discard is a plain delete — the drop was never a library
/// asset, so nothing needs a provenance record — but it is irreversible for the
/// bytes, which is why it is gated behind a confirmation.
@MainActor
@Observable
public final class DropDetailViewModel {
    /// What happened, once something did.
    public enum Outcome: Sendable, Equatable, Hashable {
        case adopted(AssetID)
        case discarded
    }

    public let drop: PendingDrop
    public private(set) var albums: [ContainerAlbum] = []
    public var destination: AlbumID?
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?
    public private(set) var outcome: Outcome?
    public private(set) var preview: DropPreviewState = .pending
    public private(set) var isWorking = false

    /// Whether the discard confirmation is on screen. The only route to
    /// ``discard()``.
    public var isConfirmingDiscard = false

    private let dropPort: any DropPort
    private let albumPort: any AlbumPort
    private let connectivity: SharingConnectivity

    public init(
        drop: PendingDrop,
        drops dropPort: any DropPort,
        albums albumPort: any AlbumPort,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        self.drop = drop
        self.dropPort = dropPort
        self.albumPort = albumPort
        self.connectivity = connectivity
    }

    // MARK: Derived state

    /// The guest's asserted filename, sanitised for display.
    ///
    /// Rendered in quotes behind an "unverified" marker by the view. It is a
    /// string a stranger typed: advisory only, never a path.
    public var claimedFilename: String? {
        GuestClaim.quoted(drop.suggestedFilename)
    }

    public var canAdopt: Bool {
        !isWorking && outcome == nil && destination != nil
    }

    // MARK: Actions

    /// Load the destinations, and start the sandboxed preview decode.
    public func load() async {
        connection = await connectivity.probe()
        do {
            albums = try await albumPort.containerAlbums()
            destination = destination ?? albums.first(where: \.isDefault)?.id ?? albums.first?.id
            phase = .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
        await decodePreview()
    }

    /// Ask the sandboxed decoder for a preview.
    ///
    /// No port carries a drop preview yet, so this resolves to ``unavailable``
    /// rather than pretending. It is a real state either way — a hostile or
    /// simply broken file lands here — and the screen is designed around it:
    /// the user can discard a drop they were never able to look at.
    public func decodePreview() async {
        preview = .decoding
        preview = .unavailable
    }

    /// Adopt into the chosen album.
    ///
    /// Adoption happens **in place**: the already-stored blob is reclassified
    /// from inbox to album asset, so the bulk bytes incur no new quota and only
    /// the metadata and provenance writes are charged.
    public func adopt() async {
        guard canAdopt, let destination else { return }
        isWorking = true
        defer { isWorking = false }
        do {
            let assetID = try await dropPort.adopt(drop.id, into: destination)
            outcome = .adopted(assetID)
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Ask for confirmation. Discarding is irreversible for the bytes.
    public func requestDiscard() {
        guard outcome == nil else { return }
        isConfirmingDiscard = true
    }

    /// Discard — **only** from a confirmed dialog.
    ///
    /// The guard is in the model rather than only in the view so a swipe
    /// action, a keyboard shortcut, or a future automation cannot reach the
    /// destructive path without the user having said yes.
    public func discard() async {
        guard isConfirmingDiscard, !isWorking, outcome == nil else { return }
        isConfirmingDiscard = false
        isWorking = true
        defer { isWorking = false }
        do {
            try await dropPort.discard(drop.id)
            outcome = .discarded
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Dismiss the confirmation without discarding.
    public func cancelDiscard() {
        isConfirmingDiscard = false
    }
}
