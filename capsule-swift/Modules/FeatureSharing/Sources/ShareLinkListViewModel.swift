import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - ShareLinkRow

/// One row in the issued-links list.
///
/// A presentation projection rather than the raw ``ShareLink`` for one reason
/// that matters: ``ShareLink/secret`` is live decryption material, and a row
/// model that carried it would put a key inside every diffable list identity,
/// every animation, and every debug print of the screen's state. The list never
/// needs the secret — only the composer, at the moment of copying, does.
public struct ShareLinkRow: Sendable, Equatable, Identifiable, Hashable {
    public var id: ShareID
    public var scope: ShareScope
    public var expiresAt: CapsuleTimestamp?
    public var revokedAt: CapsuleTimestamp?
    public var hasPassphrase: Bool
    public var isLive: Bool

    /// When the link was issued.
    ///
    /// Optional because ``SharePort`` does not yet report it, and it cannot be
    /// recovered from the URL: ``ShareLink/opaqueID`` is deliberately
    /// unstructured precisely so a link leaks no creation ordering. Wired
    /// through as an optional so the row lights up the moment the port grows the
    /// field, instead of the UI inventing a date.
    public var createdAt: CapsuleTimestamp?

    /// When the link was last resolved.
    ///
    /// Also optional, and for a stronger reason: per-recipient analytics are
    /// **out of v1 scope** (*Share Links*). The link is the credential, so the
    /// server can know that a link was used and can never know by whom — an
    /// aggregate last-used instant is the most that could ever appear here.
    public var lastUsedAt: CapsuleTimestamp?

    public init(link: ShareLink, now: CapsuleTimestamp) {
        id = link.id
        scope = link.scope
        expiresAt = link.expiresAt
        revokedAt = link.revokedAt
        hasPassphrase = link.hasPassphrase
        isLive = link.isLive(at: now)
        createdAt = nil
        lastUsedAt = nil
    }

    /// Why the link stopped resolving, for a list that keeps revoked links on
    /// record so the history stays auditable.
    public enum Lapse: Sendable, Equatable, Hashable {
        case revoked
        case expired
    }

    public var lapse: Lapse? {
        if revokedAt != nil { return .revoked }
        return isLive ? nil : .expired
    }
}

// MARK: - ShareLinkListViewModel

/// Drives the list of links this user has issued (*Share Links*).
///
/// Revoked and expired links stay on record rather than vanishing. A link
/// cannot be un-shared retroactively for someone who already opened it, so the
/// record is the only account of what was handed out — deleting the row would
/// destroy the audit trail and imply an un-sharing that did not happen.
@MainActor
@Observable
public final class ShareLinkListViewModel {
    public private(set) var active: [ShareLinkRow] = []
    public private(set) var inactive: [ShareLinkRow] = []
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?
    /// The link a revoke confirmation is pending on.
    public var pendingRevocation: ShareLinkRow?

    private let share: any SharePort
    private let connectivity: SharingConnectivity
    private let now: @Sendable () -> CapsuleTimestamp

    public init(
        share: any SharePort,
        connectivity: SharingConnectivity = SharingConnectivity(),
        now: @escaping @Sendable () -> CapsuleTimestamp = { CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970)) }
    ) {
        self.share = share
        self.connectivity = connectivity
        self.now = now
    }

    /// Load, or reload after a revocation.
    public func load() async {
        connection = await connectivity.probe()
        do {
            let instant = now()
            let rows = try await share.links().map { ShareLinkRow(link: $0, now: instant) }
            active = rows.filter(\.isLive)
            inactive = rows.filter { !$0.isLive }
            phase = rows.isEmpty ? .empty : .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Revoke, then reload so the row moves from active to the record.
    ///
    /// The serving endpoint is fail-closed but caches its decision, so the
    /// honest promise the UI makes is "within about a minute", not "instantly".
    public func revoke(_ row: ShareLinkRow) async {
        pendingRevocation = nil
        do {
            try await share.revokeLink(row.id)
            await load()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }
}
