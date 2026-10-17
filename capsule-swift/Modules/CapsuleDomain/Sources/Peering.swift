import Foundation

// MARK: - LocalPeer

/// Another of **this same user's** devices, reachable on the local network
/// (*Peering*).
///
/// Peering is not federation. Federation moves data between *accounts* on
/// different servers under capability tokens; peering moves originals between
/// one user's own devices without touching the WAN at all. It is the designated
/// degraded-mode alternative when the WAN path is persistently
/// ``ConnectionClass/adverse`` — the staged-upload index still escapes over the
/// thin link while bulk originals travel over the LAN.
public struct LocalPeer: Sendable, Equatable, Identifiable, Hashable {
    /// How much this device trusts the peer right now.
    public enum Trust: Sendable, Equatable, Hashable {
        /// Discovered on the network but not yet confirmed as the user's own
        /// device. Nothing is transferred in this state.
        case discovered
        /// Confirmed against the account's device directory. Only a device in
        /// the directory can be paired — network presence alone proves nothing.
        case paired
        /// Previously paired, now revoked in the directory.
        case revoked
    }

    /// The peer's device id, from the account's directory.
    public var id: DeviceID
    /// The peer's self-reported model.
    public var model: String
    public var platform: PlatformTag
    public var trust: Trust
    /// When it was last seen on the network.
    public var lastSeenAt: CapsuleTimestamp

    public init(
        id: DeviceID,
        model: String,
        platform: PlatformTag,
        trust: Trust,
        lastSeenAt: CapsuleTimestamp
    ) {
        self.id = id
        self.model = model
        self.platform = platform
        self.trust = trust
        self.lastSeenAt = lastSeenAt
    }

    /// Whether a transfer may be attempted with this peer.
    public var permitsTransfer: Bool {
        trust == .paired
    }
}

// MARK: - PeeringTransfer

/// One in-flight LAN transfer between two of the user's devices.
///
/// Bytes moved this way are the same encrypted, content-addressed blobs the
/// server would carry — peering changes the *path*, never the format or the
/// verification. A blob received from a peer is verified against its content
/// address exactly as one fetched from the server is, because a peer is a
/// device on a network, not a trusted authority.
public struct PeeringTransfer: Sendable, Equatable, Identifiable, Hashable {
    /// Which way the bytes are going, from this device's point of view.
    public enum Direction: Sendable, Equatable, Hashable {
        case sending
        case receiving
    }

    public var id: BlobHash
    public var peerID: DeviceID
    public var direction: Direction
    public var assetID: String
    public var blobRole: BlobRole
    public var transferredBytes: UInt64
    public var totalBytes: UInt64

    public init(
        id: BlobHash,
        peerID: DeviceID,
        direction: Direction,
        assetID: String,
        blobRole: BlobRole,
        transferredBytes: UInt64,
        totalBytes: UInt64
    ) {
        self.id = id
        self.peerID = peerID
        self.direction = direction
        self.assetID = assetID
        self.blobRole = blobRole
        self.transferredBytes = transferredBytes
        self.totalBytes = totalBytes
    }

    /// Transferred fraction, 0…1.
    public var fractionComplete: Double {
        totalBytes == 0 ? 0 : min(1, Double(transferredBytes) / Double(totalBytes))
    }
}
