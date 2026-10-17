import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Lifecycle

/// Trash, restore, and purge — the three writes that decide whether a user ever
/// loses a photograph.
public extension MockLibraryStore {
    /// Soft-delete into the trash with a signed retention window.
    ///
    /// The deadline is a **floor**, not a promise: in the real system it is
    /// signed into the `delete` manifest so the server can neither accelerate
    /// the purge nor delay it past a restore. Deriving it here from the same
    /// arithmetic means the countdown a user sees in the mock is the countdown
    /// the real one shows.
    func moveToTrash(_ assetIDs: [AssetID], retentionDays: Int?) async throws {
        let days = retentionDays ?? TrashEntry.defaultRetentionDays
        let deletedAt = now
        let deadline = CapsuleTimestamp(epochSeconds: deletedAt.epochSeconds + Int64(days) * 86400)
        await mutate(assetIDs) { patch in
            patch.isDeleted = true
            patch.deletedAt = deletedAt
            patch.retentionUntil = deadline
            patch.isUserHidden = nil
        }
        await announceReload()
    }

    /// Restore from trash within the retention window.
    ///
    /// Appends a provenance record; the original `delete` is **not** removed, so
    /// the chain keeps "deleted on X, restored on Y". Refused past the deadline
    /// rather than silently succeeding, because a restore the server would
    /// reject is a lie the client should not tell.
    func restoreFromTrash(_ assetIDs: [AssetID]) async throws {
        for assetID in assetIDs {
            guard let asset = engine.asset(for: assetID), asset.isDeleted else { continue }
            let patch = currentOverlay.patch(for: assetID)
            let deadline = patch?.retentionUntil
                ?? CapsuleTimestamp(
                    epochSeconds: (asset.deletedAt?.epochSeconds ?? now.epochSeconds)
                        + Int64(TrashEntry.defaultRetentionDays) * 86400
                )
            guard now < deadline else {
                throw CapsuleError(
                    code: .uploadInvalidAction,
                    detail: "CapsuleMock: retention window has elapsed"
                )
            }
        }
        await mutate(assetIDs) { patch in
            patch.isDeleted = false
            patch.deletedAt = nil
            patch.retentionUntil = nil
        }
        await announceReload()
    }

    /// Permanently purge at the user's explicit request.
    ///
    /// Irreversible for the bytes; the provenance chain survives as a
    /// tombstone-with-history, which is why ``LibraryPort/provenanceChain(for:)``
    /// keeps answering for an asset ``LibraryPort/asset(for:)`` no longer
    /// returns.
    func purge(_ assetIDs: [AssetID]) async throws {
        await mutate(assetIDs) { patch in
            patch.isPurged = true
            patch.isDeleted = true
        }
        await announceReload()
    }

    /// The trash, with each entry's retention deadline.
    func trashEntries(offset: Int, limit: Int) async throws -> Page<TrashEntry> {
        engine.trashEntries(
            offset: offset,
            limit: limit,
            retentionDays: TrashEntry.defaultRetentionDays
        )
    }
}
