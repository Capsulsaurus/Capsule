import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - QuotaPort

extension MockTransferStore: QuotaPort {
    public func status() async throws -> QuotaStatus {
        currentQuota
    }

    /// Whether a prospective upload would be admitted.
    ///
    /// Checked **before** starting one rather than discovered mid-import, which
    /// is the difference between a user being told they need to free space and a
    /// user watching four hundred photographs fail one at a time.
    ///
    /// Session creation is the only hard enforcement point: once a session is
    /// open the declared size is the cap and it is allowed to complete.
    public func wouldAdmit(additionalBytes: UInt64) async throws -> Bool {
        let quota = currentQuota
        guard quota.state.permitsNewUploads else { return false }
        guard !quota.isUnlimited else { return true }
        return quota.used + additionalBytes <= quota.hardLimit
    }

    public nonisolated func changes() -> AsyncStream<QuotaStatus> {
        quotaChanges.subscribe()
    }

    /// Recompute the state from the numbers after usage moves.
    ///
    /// ``QuotaState/suspended`` is an administrative fact no client can compute,
    /// so it is preserved rather than derived away — a client that recomputed a
    /// suspension out of existence would show a working library to someone whose
    /// account is locked.
    func applyUsageChange(delta: Int64) async {
        var quota = currentQuota
        let updated = Int64(quota.used) + delta
        quota.used = UInt64(max(0, updated))
        quota.state = QuotaStatus.derivedState(
            used: quota.used,
            softLimit: quota.softLimit,
            hardLimit: quota.hardLimit,
            hardExceededSince: quota.hardExceededSince,
            now: configuration.clock.now,
            isSuspended: currentQuota.state == .suspended
        )
        setQuota(quota)
        await quotaChanges.send(quota)
    }
}
