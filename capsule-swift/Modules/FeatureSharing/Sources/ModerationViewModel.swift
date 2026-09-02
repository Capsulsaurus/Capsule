import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - ModerationViewModel

/// Blocks, reports, untrusted origins, the audit log, and appeals
/// (*Moderation*).
///
/// The screen exists because moderation in an end-to-end-encrypted system is
/// necessarily *visible*: the server cannot scan content, so every action it
/// does take is a decision somebody made, and the design's global rule is that
/// each one produces a provenance record the affected user can see. This model's
/// job is to make sure none of those five things is quietly missing.
@MainActor
@Observable
public final class ModerationViewModel {
    public private(set) var blocks: [BlockEntry] = []
    public private(set) var reports: [ModerationReport] = []
    public private(set) var auditEntries: [ModerationAuditEntry] = []
    public private(set) var appeals: [ModerationAppeal] = []
    public private(set) var untrustedOrigins: [UntrustedOrigin] = []
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?
    /// The block a confirmation is pending on.
    public var pendingUnblock: BlockEntry?

    private let moderation: any ModerationPort
    private let records: any ModerationRecordPort
    private let originPolicy: any UntrustedOriginPolicy
    private let connectivity: SharingConnectivity

    public init(
        moderation: any ModerationPort,
        records: any ModerationRecordPort,
        originPolicy: any UntrustedOriginPolicy,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        self.moderation = moderation
        self.records = records
        self.originPolicy = originPolicy
        self.connectivity = connectivity
    }

    // MARK: Derived state

    /// The appeal filed against one audit entry, if any.
    public func appeal(for entry: ModerationAuditEntry) -> ModerationAppeal? {
        appeals.first { $0.entryID == entry.id }
    }

    /// Whether an entry can still be appealed and has not been already.
    public func canAppeal(_ entry: ModerationAuditEntry) -> Bool {
        entry.isAppealable && appeal(for: entry) == nil
    }

    /// Origins whose content is being withheld pending an explicit decision.
    public var pendingConsentOrigins: [UntrustedOrigin] {
        untrustedOrigins.filter { !$0.isConsented }
    }

    // MARK: Actions

    public func load() async {
        connection = await connectivity.probe()
        do {
            blocks = try await moderation.blocks()
            auditEntries = try await records.auditEntries()
            appeals = try await records.appeals()
            untrustedOrigins = try await originPolicy.untrustedOrigins()
            phase = .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Block a user or a peer server.
    ///
    /// Per-origin and local: it drops that origin's constituent from *this
    /// viewer's* aggregated albums and stops pulls from it, without affecting
    /// any other participant's view — and without clawing back epoch keys the
    /// blocked party already holds. The UI says so; blocking is not retroactive
    /// unseeing.
    public func block(_ subject: BlockEntry.Subject) async {
        do {
            try await moderation.block(subject)
            await load()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    public func unblock(_ entry: BlockEntry) async {
        pendingUnblock = nil
        do {
            try await moderation.unblock(entry.id)
            await load()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// File a report.
    ///
    /// Rate-limited per reporter and per subject — backpressure is what defeats
    /// mass-report abuse — so a refusal here is a normal outcome and surfaces as
    /// its own code rather than a generic failure.
    public func report(_ subject: ModerationReport.Subject, reason: ModerationReport.Reason) async {
        do {
            let filed = try await moderation.report(subject, reason: reason)
            reports.append(filed)
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Grant or withdraw consent to load from an untrusted origin.
    ///
    /// Default-deny: until this is granted the client skips those assets, and
    /// the entries stay withheld rather than being removed. Granting is
    /// explicitly an acceptance of risk, which the UI states next to the
    /// control.
    public func setConsent(_ granted: Bool, for origin: String) async {
        do {
            try await originPolicy.setConsent(granted, for: origin)
            untrustedOrigins = try await originPolicy.untrustedOrigins()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Appeal a moderation action.
    public func appealEntry(_ entry: ModerationAuditEntry) async {
        guard canAppeal(entry) else { return }
        do {
            let appeal = try await records.submitAppeal(for: entry.id)
            appeals.append(appeal)
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }
}
