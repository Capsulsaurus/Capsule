import CapsuleFoundation
import Foundation

// MARK: - AlbumRole

/// A member's capability in a container album
/// (*Asset Organization — Container Albums*).
///
/// The three capabilities are cryptographic, not advisory: read holds the AMK
/// only, write also holds the write-tier key, admin also the admin-tier key. A
/// role change is an MLS commit and bumps the AMK epoch — which is why the UI
/// must never let a role look changed before the commit lands.
public enum AlbumRole: ClosedWireEnum {
    /// AMK only — can decrypt, cannot author.
    case read
    /// AMK plus the write-tier key.
    case write
    /// Also the admin-tier key: invites, removals, policy.
    case admin
    case unknown(String)

    public static let knownCases: [AlbumRole] = [.read, .write, .admin]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .read: "read"
        case .write: "write"
        case .admin: "admin"
        case let .unknown(raw): raw
        }
    }

    /// Whether this role may author writes into the album.
    public var canWrite: Bool {
        self == .write || self == .admin
    }

    /// Whether this role may change membership or policy.
    public var canAdminister: Bool {
        self == .admin
    }
}

// MARK: - AlbumMember

/// One member of a container album.
///
/// Identified by handle (`user@server.tld`), because membership spans servers —
/// a federated member has no local user row to point at.
public struct AlbumMember: Sendable, Equatable, Identifiable, Hashable {
    /// The member's `user@server.tld` handle.
    public var handle: String
    /// Their capability in this album.
    public var role: AlbumRole

    public var id: String { handle }

    public init(handle: String, role: AlbumRole) {
        self.handle = handle
        self.role = role
    }

    /// The home server half of the handle, when it parses. Drives the
    /// per-origin availability surfaces on an aggregated album.
    public var homeServer: String? {
        handle.split(separator: "@", maxSplits: 1).count == 2
            ? String(handle.split(separator: "@", maxSplits: 1)[1])
            : nil
    }
}

// MARK: - AlbumPolicy

/// How much AMK history a new joiner receives (*MLS — History Delivery*).
public enum HistoryPolicy: ClosedWireEnum {
    /// Every epoch — the joiner can decrypt the album's whole past.
    case full
    /// A bounded range of recent epochs only.
    case capped
    case unknown(String)

    public static let knownCases: [HistoryPolicy] = [.full, .capped]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .full: "full"
        case .capped: "capped"
        case let .unknown(raw): raw
        }
    }
}

/// The policy fixed at album creation.
///
/// **Fixed at creation and changed only through an album upgrade ceremony,
/// never ad hoc.** The UI must therefore present these as creation-time
/// decisions, not as settings toggles — an editable-looking control over an
/// immutable value is a promise the system cannot keep.
public struct AlbumPolicy: Sendable, Equatable, Hashable {
    /// How much history a joiner gets.
    public var historyPolicy: HistoryPolicy
    /// Default trash retention for deletes in this album, in days.
    public var retentionDays: Int
    /// The album's pinned date-based wire protocol version (`YYYY-MM-DD`).
    /// Constrains which sidecar schemas may be written into it.
    public var protocolVersion: String

    public init(historyPolicy: HistoryPolicy, retentionDays: Int, protocolVersion: String) {
        self.historyPolicy = historyPolicy
        self.retentionDays = retentionDays
        self.protocolVersion = protocolVersion
    }
}

// MARK: - ContainerAlbum

/// A container album — the **real cryptographic unit**
/// (*Asset Organization — Container Albums*).
///
/// An album *is* an MLS group, and it is the only sharing and access-control
/// boundary in the system. Every asset lives in exactly one; a move is a signed
/// lifecycle action naming `(asset, target album, epoch)` and is idempotent, so
/// replaying it finds the target state already in place.
///
/// The UI calls two different things "albums". This is the one that owns assets
/// and holds keys; ``ViewAlbum`` is the derived, key-free presentation. Keeping
/// them separate types is what stops a view from ever being offered as an
/// import destination.
public struct ContainerAlbum: Sendable, Equatable, Identifiable, Hashable {
    /// The album's identity.
    public var id: AlbumID
    /// The user-assigned name, or `nil` for the **default album** — a de facto,
    /// nameless container that exists for every owner from first-device
    /// enrollment onward, so an import always has somewhere to land.
    public var name: String?
    /// The asset shown as the album's cover.
    public var coverAssetID: AssetID?
    /// How many assets it holds.
    public var count: Int
    /// The current AMK epoch. Bumps on every membership or role change.
    public var epoch: UInt32
    /// Creation-time policy.
    public var policy: AlbumPolicy
    /// Members and their capabilities.
    public var members: [AlbumMember]
    /// Whether this is the owner's currently-designated default album.
    ///
    /// The designation is a **non-secret server-side owner pointer**, not
    /// security-bearing: a write still requires real album write capability.
    public var isDefault: Bool

    public init(
        id: AlbumID,
        name: String?,
        coverAssetID: AssetID? = nil,
        count: Int,
        epoch: UInt32,
        policy: AlbumPolicy,
        members: [AlbumMember] = [],
        isDefault: Bool = false
    ) {
        self.id = id
        self.name = name
        self.coverAssetID = coverAssetID
        self.count = count
        self.epoch = epoch
        self.policy = policy
        self.members = members
        self.isDefault = isDefault
    }

    /// Whether the album is shared with anyone beyond the owner.
    public var isShared: Bool {
        members.count > 1
    }

    /// Whether the album may be deleted. The **current default cannot be
    /// deleted while designated** — the user must repoint first — so import
    /// always has a home.
    public var isDeletable: Bool {
        !isDefault
    }
}

// MARK: - ViewAlbum

/// A view album — a derived, **key-free** presentation
/// (*Asset Organization — System & Smart Albums*).
///
/// A view is not an MLS group, holds no AMK, owns no assets, and is **not** a
/// sharing or access-control boundary. Membership is computed client-side over
/// the assets the viewer can already decrypt, never stored — so editing a view,
/// or an asset's attributes, moves and re-encrypts nothing.
public struct ViewAlbum: Sendable, Equatable, Identifiable, Hashable {
    /// What kind of view this is.
    public enum Kind: Sendable, Equatable, Hashable {
        /// Built-in and implicit. **All** is the union over the viewer's
        /// containers — which is exactly why the default album matters, since
        /// an import always enters *some* container and so shows up here.
        case system(SystemView)
        /// A user-defined predicate over sidecar and AI-derived fields.
        case smart(SmartAlbumID)
        /// The aggregated federated album: a group id spanning constituents on
        /// different home servers.
        case aggregated(AlbumGroupID)
    }

    /// The built-in views.
    public enum SystemView: Sendable, Equatable, Hashable, CaseIterable {
        /// Every asset the viewer can see.
        case all
        /// Assets in the retention window. Behind a fresh-local-auth gate.
        case trash
        /// User-hidden assets. Behind the same fresh-local-auth gate as trash.
        case hidden
        /// Assets held for a human decision.
        case quarantine
    }

    public var id: AlbumID
    public var kind: Kind
    /// The asset count, computed. `nil` while it is still being evaluated —
    /// a view's membership is a query, not a stored number.
    public var count: Int?
    public var coverAssetID: AssetID?

    public init(id: AlbumID, kind: Kind, count: Int? = nil, coverAssetID: AssetID? = nil) {
        self.id = id
        self.kind = kind
        self.count = count
        self.coverAssetID = coverAssetID
    }

    /// A view is never an import destination — resolution **always** lands on a
    /// container. Exposed so a destination picker can filter structurally
    /// rather than by convention.
    public var canReceiveImports: Bool { false }

    /// Whether opening this view requires fresh local authentication.
    public var requiresFreshLocalAuth: Bool {
        if case let .system(view) = kind {
            return view == .trash || view == .hidden
        }
        return false
    }
}
