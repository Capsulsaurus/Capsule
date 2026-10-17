import Foundation
import Testing

import CapsuleDomain

/// The five quota states and the transitions between them (*Quota — Threshold
/// Model*).
@Suite("Quota states follow the documented thresholds")
struct QuotaStateTests {
    private let soft: UInt64 = 800
    private let hard: UInt64 = 1000

    private func state(
        used: UInt64,
        exceededSince: CapsuleTimestamp? = nil,
        now: CapsuleTimestamp = Fixtures.epoch,
        suspended: Bool = false
    ) -> QuotaState {
        QuotaStatus.derivedState(
            used: used,
            softLimit: soft,
            hardLimit: hard,
            hardExceededSince: exceededSince,
            now: now,
            isSuspended: suspended
        )
    }

    // MARK: Thresholds

    @Test("below the soft limit is within quota")
    func belowSoft() {
        #expect(state(used: 0) == .withinQuota)
        #expect(state(used: 799) == .withinQuota)
    }

    @Test("the soft limit is inclusive — reaching it warns")
    func softIsInclusive() {
        #expect(state(used: 800) == .softWarning)
        #expect(state(used: 999) == .softWarning)
    }

    @Test("the hard limit is inclusive — reaching it exceeds")
    func hardIsInclusive() {
        #expect(state(used: 1000) == .hardExceeded)
        #expect(state(used: 5000) == .hardExceeded)
    }

    @Test("grace expires only after the window elapses over the hard limit")
    func graceExpiry() {
        let since = Fixtures.epoch
        let dayThirteen = Fixtures.time(offsetSeconds: 13 * 86400)
        let dayFifteen = Fixtures.time(offsetSeconds: 15 * 86400)

        #expect(state(used: 1200, exceededSince: since, now: dayThirteen) == .hardExceeded)
        #expect(state(used: 1200, exceededSince: since, now: dayFifteen) == .graceExpired)
    }

    @Test("without a recorded crossing time the state cannot escalate past hard-exceeded")
    func noCrossingTimeStaysHardExceeded() {
        // A missing timestamp must not be read as "exceeded forever" — that
        // would silently lock a user's metadata edits on a fresh sign-in.
        #expect(state(used: 1200, exceededSince: nil, now: Fixtures.time(offsetSeconds: 365 * 86400)) == .hardExceeded)
    }

    @Test("suspension overrides every threshold, including a clean usage figure")
    func suspensionOverrides() {
        #expect(state(used: 0, suspended: true) == .suspended)
        #expect(state(used: 5000, suspended: true) == .suspended)
    }

    @Test("freeing space walks the ladder back down")
    func recoveryTransitions() {
        let since = Fixtures.epoch
        let later = Fixtures.time(offsetSeconds: 20 * 86400)
        #expect(state(used: 1200, exceededSince: since, now: later) == .graceExpired)
        // Deleting below the hard limit lifts grace immediately, without
        // waiting out any window.
        #expect(state(used: 900, exceededSince: since, now: later) == .softWarning)
        #expect(state(used: 100, exceededSince: since, now: later) == .withinQuota)
    }

    // MARK: Capability gates

    @Test("uploads are refused from hard-exceeded onward")
    func uploadGate() {
        #expect(QuotaState.withinQuota.permitsNewUploads)
        #expect(QuotaState.softWarning.permitsNewUploads)
        #expect(!QuotaState.hardExceeded.permitsNewUploads)
        #expect(!QuotaState.graceExpired.permitsNewUploads)
        #expect(!QuotaState.suspended.permitsNewUploads)
    }

    @Test("metadata edits survive hard-exceeded but not grace-expired")
    func metadataGrowthGate() {
        #expect(QuotaState.hardExceeded.permitsMetadataGrowth)
        #expect(!QuotaState.graceExpired.permitsMetadataGrowth)
    }

    @Test("deleting is always admitted except under suspension")
    func reclaimingGate() {
        // The product promise: a user can always delete their way back under
        // quota. If grace-expired blocked deletes, a full account would be
        // permanently full.
        for quotaState in [QuotaState.withinQuota, .softWarning, .hardExceeded, .graceExpired] {
            #expect(quotaState.permitsReclaimingWrites)
        }
        #expect(!QuotaState.suspended.permitsReclaimingWrites)
    }

    @Test("an unknown state from a newer server fails closed on writes")
    func unknownStateFailsClosed() {
        let future = QuotaState(rawValue: "billing_dispute")
        #expect(!future.isKnown)
        #expect(!future.permitsNewUploads)
        #expect(!future.permitsMetadataGrowth)
    }

    // MARK: Status arithmetic

    @Test("remaining floors at zero and never underflows")
    func remainingFloors() {
        // `UInt64` subtraction traps on underflow, so an over-quota account
        // would crash the settings screen rather than show a full bar.
        let over = QuotaStatus(used: 1200, softLimit: soft, hardLimit: hard, state: .hardExceeded)
        #expect(over.remaining == 0)
        #expect(over.fractionUsed == 1)

        let under = QuotaStatus(used: 400, softLimit: soft, hardLimit: hard, state: .withinQuota)
        #expect(under.remaining == 600)
    }

    @Test("an unlimited deployment reports zero usage fraction rather than a sliver")
    func unlimitedDeployment() {
        let unlimited = QuotaStatus(used: 5000000, softLimit: .max, hardLimit: .max, state: .withinQuota)
        #expect(unlimited.isUnlimited)
        #expect(unlimited.fractionUsed == 0)
    }

    @Test("the local breakdown excludes unreleased originals from reclaimable bytes")
    func reclaimableExcludesUnreleasedOriginals() {
        // A device-owned original not yet confirmed durable is the only copy
        // that exists. Counting it as reclaimable is how a cache-clearing
        // feature deletes a photo permanently.
        let breakdown = LocalStorageBreakdown(
            bytesByTier: [.thumbnail: 100, .preview: 400, .original: 1000],
            trashBytes: 250,
            unreleasedOriginalBytes: 1000
        )
        #expect(breakdown.totalBytes == 1500)
        #expect(breakdown.reclaimableBytes == 500)
        // Trash counts fully — it is why the UI highlights the segment.
        #expect(breakdown.trashBytes == 250)
    }
}
