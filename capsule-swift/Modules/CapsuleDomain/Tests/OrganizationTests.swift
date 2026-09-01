import Foundation
import Testing

import CapsuleDomain

/// Derived group cull state, and the three-flags rule.
@Suite("A group's cull state is derived from its members, never stored")
struct GroupCullStateTests {
    @Test("every member rejected reads as all-rejected")
    func allRejected() {
        #expect(GroupCullState(members: [.reject, .reject, .reject]) == .allRejected)
        #expect(GroupCullState(members: [.reject]) == .allRejected)
    }

    @Test("a single pick protects the whole group from a reject sweep")
    func anyPickWins() {
        // Precedence matters: a group with one pick and nine rejects is
        // `anyPick`, not `mixed` and certainly not `allRejected`. Getting this
        // backwards batch-deletes a burst the user explicitly kept a frame from.
        #expect(GroupCullState(members: [.pick, .reject, .reject]) == .anyPick)
        #expect(GroupCullState(members: [.pick, .neutral]) == .anyPick)
        #expect(GroupCullState(members: [.pick]) == .anyPick)
    }

    @Test("neutral and reject with no pick reads as mixed")
    func mixed() {
        #expect(GroupCullState(members: [.neutral, .reject]) == .mixed)
        #expect(GroupCullState(members: [.neutral, .neutral]) == .mixed)
    }

    @Test("an empty group is empty, not all-rejected")
    func emptyGroup() {
        // `allSatisfy` on an empty collection is vacuously true, so a naive
        // implementation reports an empty stack as fully rejected — and offers
        // to delete nothing, loudly.
        #expect(GroupCullState(members: []) == .empty)
    }

    @Test("the derived state ignores member order")
    func orderIndependent() {
        let members: [CullFlag] = [.neutral, .pick, .reject]
        #expect(GroupCullState(members: members) == GroupCullState(members: members.reversed()))
    }

    @Test("an unknown flag from a newer client does not read as a pick")
    func unknownFlagIsNotAPick() {
        let state = GroupCullState(members: [.unknown("maybe"), .neutral])
        #expect(state == .mixed)
        #expect(state != .anyPick)
    }
}

/// Parity rule 5: trash, stack-collapse, and user-hidden are three different
/// flags.
@Suite("Trash, stack-collapse, and user-hidden are three distinct flags")
struct VisibilityFlagTests {
    @Test("each flag alone removes an asset from the default timeline")
    func eachFlagExcludes() {
        #expect(Fixtures.libraryAsset(id: "live", captureSeconds: 100).appearsInDefaultTimeline)
        #expect(!Fixtures.libraryAsset(id: "trashed", captureSeconds: 100, isDeleted: true).appearsInDefaultTimeline)
        #expect(!Fixtures.libraryAsset(id: "stacked", captureSeconds: 100, isStackHidden: true).appearsInDefaultTimeline)
        #expect(!Fixtures.libraryAsset(id: "hidden", captureSeconds: 100, isUserHidden: true).appearsInDefaultTimeline)
    }

    @Test("each view admits exactly the flag it is for, and no other")
    func viewsAdmitTheirOwnFlag() {
        let trashed = Fixtures.libraryAsset(id: "trashed", captureSeconds: 100, isDeleted: true)
        let stacked = Fixtures.libraryAsset(id: "stacked", captureSeconds: 100, isStackHidden: true)
        let hidden = Fixtures.libraryAsset(id: "hidden", captureSeconds: 100, isUserHidden: true)

        // Trash shows trashed assets *only* — and does not leak hidden ones,
        // which sit behind the same auth gate but are a different surface.
        #expect(TimelineQuery.trash.admitsVisibility(of: trashed))
        #expect(!TimelineQuery.trash.admitsVisibility(of: hidden))
        #expect(!TimelineQuery.trash.admitsVisibility(of: stacked))
        #expect(!TimelineQuery.trash.admitsVisibility(of: Fixtures.libraryAsset(id: "live", captureSeconds: 1)))

        // Hidden shows hidden assets, not trashed ones and not live ones.
        #expect(TimelineQuery.hidden.admitsVisibility(of: hidden))
        #expect(!TimelineQuery.hidden.admitsVisibility(of: trashed))
        #expect(!TimelineQuery.hidden.admitsVisibility(of: Fixtures.libraryAsset(id: "live", captureSeconds: 1)))

        // An expanded stack shows its members — without resurrecting anything
        // from the trash, because stack collapse is orthogonal to the slice.
        let expanded = TimelineQuery(includeStackHidden: true)
        #expect(expanded.admitsVisibility(of: stacked))
        #expect(!expanded.admitsVisibility(of: trashed))

        // An expanded stack *inside* the Trash view shows its trashed members
        // and still nothing live.
        let expandedTrash = TimelineQuery(slice: .trash, includeStackHidden: true)
        let trashedStackMember = Fixtures.libraryAsset(
            id: "both",
            captureSeconds: 100,
            isDeleted: true,
            isStackHidden: true
        )
        #expect(expandedTrash.admitsVisibility(of: trashedStackMember))
        #expect(!expandedTrash.admitsVisibility(of: stacked))
    }

    @Test("the default query admits none of the three")
    func defaultQueryExcludesAll() {
        for asset in [
            Fixtures.libraryAsset(id: "a", captureSeconds: 1, isDeleted: true),
            Fixtures.libraryAsset(id: "b", captureSeconds: 1, isStackHidden: true),
            Fixtures.libraryAsset(id: "c", captureSeconds: 1, isUserHidden: true),
        ] {
            #expect(!TimelineQuery.default.admitsVisibility(of: asset))
        }
    }
}

/// The trash retention window is a cryptographic floor, not a server setting.
@Suite("Trash retention is signed into the delete manifest")
struct TrashRetentionTests {
    @Test("an asset is restorable before its signed deadline and not after")
    func restorableWithinWindow() {
        let entry = TrashEntry(
            assetID: "asset",
            deletedAt: Fixtures.epoch,
            retentionUntil: Fixtures.time(offsetSeconds: 30 * 86400)
        )
        #expect(entry.isRestorable(at: Fixtures.time(offsetSeconds: 15 * 86400)))
        #expect(!entry.isRestorable(at: Fixtures.time(offsetSeconds: 31 * 86400)))
    }

    @Test("days remaining floors at zero rather than going negative")
    func daysRemainingFloors() {
        let entry = TrashEntry(
            assetID: "asset",
            deletedAt: Fixtures.epoch,
            retentionUntil: Fixtures.time(offsetSeconds: 30 * 86400)
        )
        #expect(entry.daysRemaining(at: Fixtures.epoch) == 30)
        #expect(entry.daysRemaining(at: Fixtures.time(offsetSeconds: 29 * 86400)) == 1)
        #expect(entry.daysRemaining(at: Fixtures.time(offsetSeconds: 40 * 86400)) == 0)
    }
}

/// Manifest structural rules, which `verify_asset` treats as terminal.
@Suite("Manifest presence-by-action rules are structural")
struct ManifestStructureTests {
    private func core(
        action: ProvenanceAction,
        prior: String?,
        metadataBlobHash: BlobHash?,
        retentionUntil: CapsuleTimestamp? = nil
    ) -> ManifestCore {
        ManifestCore(
            version: "asset-manifest/v1",
            cryptoSuiteID: 1,
            protocolVersion: "2026-07-12",
            fileID: "asset",
            albumID: "album",
            amkVersion: 1,
            ciphertextHash: BlobHash("abc"),
            plaintextSize: 10,
            chunkSize: 65536,
            metadataBlobHash: metadataBlobHash,
            createdByUser: "user",
            createdByDevice: Fixtures.deviceA,
            clientVersion: "capsule-ios/0.1.0",
            timestamp: Fixtures.epoch,
            action: action,
            priorProvenanceHash: prior,
            retentionUntil: retentionUntil
        )
    }

    @Test("prior hash is null exactly on create")
    func priorHashRule() {
        #expect(core(action: .create, prior: nil, metadataBlobHash: BlobHash("m")).isStructurallyValid)
        #expect(!core(action: .create, prior: "h", metadataBlobHash: BlobHash("m")).isStructurallyValid)
        #expect(core(action: .metadataUpdate, prior: "h", metadataBlobHash: BlobHash("m")).isStructurallyValid)
        #expect(!core(action: .metadataUpdate, prior: nil, metadataBlobHash: BlobHash("m")).isStructurallyValid)
    }

    @Test("metadata blob hash is present exactly for the three metadata-bearing actions")
    func metadataBlobRule() {
        for action in [ProvenanceAction.create, .replace, .metadataUpdate] {
            #expect(action.bindsMetadataBlob)
            let prior: String? = action.isChainRoot ? nil : "h"
            #expect(core(action: action, prior: prior, metadataBlobHash: BlobHash("m")).isStructurallyValid)
            #expect(!core(action: action, prior: prior, metadataBlobHash: nil).isStructurallyValid)
        }
        for action in [ProvenanceAction.delete, .derivativeAdd, .derivativeReplace, .trashRestore] {
            #expect(!action.bindsMetadataBlob)
            #expect(!core(action: action, prior: "h", metadataBlobHash: BlobHash("m")).isStructurallyValid)
        }
    }

    @Test("retention_until appears only on a delete")
    func retentionRule() {
        #expect(
            core(action: .delete, prior: "h", metadataBlobHash: nil, retentionUntil: Fixtures.epoch)
                .isStructurallyValid
        )
        #expect(
            !core(action: .metadataUpdate, prior: "h", metadataBlobHash: BlobHash("m"), retentionUntil: Fixtures.epoch)
                .isStructurallyValid
        )
    }

    @Test("delete and trash-restore are admitted even when quota grace has expired")
    func reclaimingActionsAlwaysAdmitted() {
        // A user must be able to delete their way back under quota.
        #expect(ProvenanceAction.delete.isAdmittedWhenGraceExpired)
        #expect(ProvenanceAction.trashRestore.isAdmittedWhenGraceExpired)
        #expect(!ProvenanceAction.metadataUpdate.isAdmittedWhenGraceExpired)
        #expect(!ProvenanceAction.create.isAdmittedWhenGraceExpired)
    }
}
