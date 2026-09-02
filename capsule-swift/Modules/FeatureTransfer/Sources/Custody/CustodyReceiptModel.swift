import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - CustodyVerdict

/// Whether this device may stop holding the only copy.
///
/// The standing rule is three checks, all required, before any post-write local
/// cleanup (*Storage Verification — Verify Before Destroy*): the client
/// **holds and has verified the custody receipt**, `/storage/verify` reports
/// `durable = true`, and the verdict is fresh. This enum is that rule made
/// unrepresentable-if-wrong: only ``releasable`` carries a destructive
/// affordance, and it cannot be constructed without all three.
public enum CustodyVerdict: Sendable, Equatable {
    /// No verdict has been taken yet.
    case unchecked
    /// The server has not confirmed custody. **Never a destructive action** —
    /// the client keeps the copy, retries with backoff, and says so.
    case notYetConfirmed(missing: [BlobVerdict])
    /// No receipt is held. A server that withholds receipts never becomes the
    /// sole holder of an only copy, so this is a refusal even when the verdict
    /// says durable.
    case receiptMissing
    /// Durable, but the verdict is older than the freshness bound. A stale
    /// `durable` must never authorise dropping an only copy; re-verify first.
    case confirmedButStale
    /// Receipt held, `durable = true`, verdict fresh. The only releasable state.
    case releasable

    /// Whether a destructive affordance may be shown at all.
    public var permitsRelease: Bool { self == .releasable }
}

// MARK: - CustodyReceiptModel

/// Drives ``CustodyReceiptView``.
///
/// Design doc: *Storage Verification — Custody Receipts, Proof of Loss, Verify
/// Before Destroy*.
@MainActor
@Observable
public final class CustodyReceiptModel {
    public private(set) var phase: ScreenPhase = .loading
    /// The receipts held for this asset, newest sequence first.
    public private(set) var receipts: [CustodyReceipt] = []
    /// The point-in-time storage verdict, when one has been taken.
    public private(set) var verification: StorageVerification?
    /// Whether the user asked for the expensive, rate-limited deep check.
    public private(set) var isDeepVerified = false
    public private(set) var isBusy = false

    public let assetID: AssetID
    private let uploads: any UploadPort
    private let storage: any StoragePort
    private let clock: TransferClock
    private var connection: ConnectionClass = .unmetered

    public init(
        assetID: AssetID,
        uploads: any UploadPort,
        storage: any StoragePort,
        clock: TransferClock = .system
    ) {
        self.assetID = assetID
        self.uploads = uploads
        self.storage = storage
        self.clock = clock
    }

    // MARK: Derived

    /// The release gate, resolved.
    public var verdict: CustodyVerdict {
        guard let verification else { return .unchecked }
        guard !receipts.isEmpty else { return .receiptMissing }
        guard verification.durable else { return .notYetConfirmed(missing: verification.missingBlobs) }
        return verification.authorisesRelease(at: clock.now) ? .releasable : .confirmedButStale
    }

    /// How long a verdict stays good for, for the freshness caption.
    public var freshnessSeconds: Int64 { StorageVerification.verdictFreshnessSeconds }

    /// A receipt at sequence *N* proves the server's log holds at least *N*
    /// entries, which is what bounds silent truncation. Worth stating on screen:
    /// it is the one property a user can act on without a cryptographer.
    public var highestReceiptSequence: UInt64? { receipts.map(\.receiptSequence).max() }

    /// Whether the receipt log chains — every receipt after the first carries
    /// the prior receipt's hash.
    public var isChained: Bool {
        receipts.count <= 1 || receipts.dropFirst().allSatisfy { $0.priorReceiptHash != nil }
    }

    // MARK: Loading

    public func load() async {
        await reload()
    }

    public func reload() async {
        do {
            let fetched = try await uploads.custodyReceipts(for: assetID)
            receipts = fetched.sorted { $0.receiptSequence > $1.receiptSequence }
            phase = receipts.isEmpty ? .empty : .ready
            await verify(deep: false)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    /// Take a fresh verdict.
    ///
    /// - Parameter deep: the expensive form, which the server prices and rate
    ///   limits, so it is only ever explicit.
    public func verify(deep: Bool) async {
        isBusy = true
        defer { isBusy = false }
        do {
            let verdicts = try await storage.verify(assetIDs: [assetID], deep: deep)
            verification = verdicts.first
            isDeepVerified = deep
            if phase.hasContent || !receipts.isEmpty { phase = .ready }
        } catch let error as CapsuleError {
            connection = error.code == .blobPendingUpload ? .offline : connection
            phase = ScreenPhase.resolve(error, connection: connection)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    /// Release this device's local copy.
    ///
    /// Guarded twice: the button is only offered in ``CustodyVerdict/releasable``,
    /// and the call is refused here as well. A non-durable verdict never
    /// triggers a destructive action — the client keeps the copy and surfaces
    /// "not yet confirmed on server".
    public func releaseLocalCopy() async {
        guard verdict.permitsRelease else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            try await storage.releaseLocalCopies(for: [assetID])
            await verify(deep: false)
        } catch let error as CapsuleError {
            phase = .failed(error)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }
}
