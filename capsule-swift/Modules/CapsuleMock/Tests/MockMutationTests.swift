import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Mutations

/// Writes must actually write.
///
/// A mock that silently dropped them would make every screen a lie: a rating
/// that springs back on scroll is worse than no rating control, because it
/// teaches the reviewer to distrust what they see. So every case here does the
/// write, reads it back, and — where a port publishes one — waits for the change
/// event a view model would be re-reading on.
@Suite("Mutations round-trip and publish")
struct MockMutationTests {
    private func makeEnvironment(assetCount: Int = 400) -> MockEnvironment {
        var configuration = MockConfiguration.make(scenario: .healthy)
        configuration.profile.assetCount = assetCount
        return MockEnvironment(configuration: configuration)
    }

    /// Registration hops onto the broadcaster's actor, so a write issued in the
    /// same turn as `changes()` could outrun it. Waiting for the subscription to
    /// land makes the test deterministic without weakening what it asserts.
    private func awaitSubscriber(_ broadcaster: ChangeBroadcaster<some Sendable>) async {
        for _ in 0 ..< 200 {
            if await broadcaster.subscriberCount > 0 { return }
            await Task.yield()
        }
    }

    @Test("Rating a set of assets is visible to later reads")
    func ratingRoundTrips() async throws {
        let environment = makeEnvironment()
        let subjects = try await environment.library.assets(matching: .default, offset: 0, limit: 3).items
        try await environment.organize.setRating(5, for: subjects.map(\.id))
        for subject in subjects {
            #expect(try await environment.library.asset(for: subject.id)?.rating == 5)
        }
        let filtered = try await environment.library.assetCount(matching: TimelineQuery(minimumRating: 5))
        #expect(filtered >= 3)
    }

    @Test("A write emits on the library change stream")
    func writesEmitChanges() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        let stream = environment.library.changes()
        await awaitSubscriber(environment.libraryStore.libraryChanges)
        try await environment.organize.setRating(3, for: [subject.id])
        var iterator = stream.makeAsyncIterator()
        let change = await iterator.next()
        #expect(change == .assetsChanged(dayKeys: [subject.dayKey]))
    }

    /// Rating and cull are separate fields: a reject can carry three stars, and
    /// a control that conflated them would force a lossy workflow.
    @Test("Cull and rating are independent")
    func cullAndRatingAreOrthogonal() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        try await environment.organize.setRating(3, for: [subject.id])
        try await environment.organize.setCull(.reject, for: [subject.id])
        let updated = try await environment.library.asset(for: subject.id)
        #expect(updated?.rating == 3)
        #expect(updated?.cull == .reject)
    }

    /// Trash is not deletion and not hiding — the three flags stay distinct, and
    /// the two aggregates move in opposite directions by the same amount.
    @Test("Trashing moves an asset between slices")
    func trashingMovesBetweenSlices() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        let liveBefore = try await environment.library.assetCount(matching: .default)
        let trashBefore = try await environment.library.assetCount(matching: .trash)

        try await environment.organize.moveToTrash([subject.id], retentionDays: nil)
        #expect(try await environment.library.assetCount(matching: .default) == liveBefore - 1)
        #expect(try await environment.library.assetCount(matching: .trash) == trashBefore + 1)
        #expect(try await environment.library.asset(for: subject.id)?.isDeleted == true)
        #expect(try await environment.library.dayCounts(matching: .default).totalCount == liveBefore - 1)

        let entries = try await environment.organize.trashEntries(offset: 0, limit: 100)
        let subjectUUID = MockAssetRef.decode(subject.id)?.uuidString(seed: environment.configuration.seed)
        #expect(entries.items.contains { $0.assetID == subjectUUID })

        try await environment.organize.restoreFromTrash([subject.id])
        #expect(try await environment.library.assetCount(matching: .default) == liveBefore)
        #expect(try await environment.library.assetCount(matching: .trash) == trashBefore)
        #expect(try await environment.library.dayCounts(matching: .default).totalCount == liveBefore)
    }

    /// Purging removes the asset and keeps its chain — the
    /// tombstone-with-history rule.
    @Test("Emptying the trash purges the bytes and keeps the history")
    func purgeKeepsProvenance() async throws {
        let environment = makeEnvironment()
        let entries = try await environment.organize.trashEntries(offset: 0, limit: 5)
        #expect(!entries.items.isEmpty)
        let trashed = try await environment.library.assets(matching: .trash, offset: 0, limit: 5).items
        try await environment.organize.purge(trashed.map(\.id))
        for subject in trashed {
            #expect(try await environment.library.asset(for: subject.id) == nil)
            let chain = try await environment.library.provenanceChain(for: subject.id)
            #expect(!chain.isEmpty)
        }
        #expect(try await environment.library.assetCount(matching: .trash) == 0)
    }

    /// A remove naming an add this replica never observed is **rejected**, not
    /// ignored — the "remove an element you never added" defence.
    @Test("Tags round-trip and an unobserved remove is rejected")
    func tagEditing() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        try await environment.organize.addUserTag("portfolio", to: [subject.id])
        #expect(try await environment.library.asset(for: subject.id)?.tagsUser.contains("portfolio") == true)

        let sidecar = try await environment.library.sidecar(for: subject.id)
        let entry = sidecar?.tagsUser.entries.first { $0.element == "portfolio" }
        #expect(entry != nil)
        if let entry {
            try await environment.organize.removeUserTag(addID: entry.addID, from: subject.id)
            #expect(try await environment.library.asset(for: subject.id)?.tagsUser.contains("portfolio") == false)
        }

        await #expect(throws: UnobservedRemove.self) {
            try await environment.organize.removeUserTag(
                addID: AddID(deviceID: DeviceID("nobody"), counter: 1),
                from: subject.id
            )
        }
    }

    /// The displaced caption is kept, not clobbered — the surface the LWW
    /// superseded log exists for.
    @Test("A replaced caption is preserved and restorable")
    func captionSupersession() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        try await environment.organize.setCaption("first", for: subject.id)
        try await environment.organize.setCaption("second", for: subject.id)
        #expect(try await environment.library.asset(for: subject.id)?.caption == "second")
        let sidecar = try await environment.library.sidecar(for: subject.id)
        let displaced = sidecar?.supersededCaptions.first { $0.value == "first" }
        #expect(displaced != nil)
        if let displaced {
            try await environment.organize.restoreCaption(displaced, for: subject.id)
            #expect(try await environment.library.asset(for: subject.id)?.caption == "first")
        }
    }

    @Test("Creating an album is visible and emits on the album stream")
    func albumCreation() async throws {
        let environment = makeEnvironment()
        let stream = environment.albums.changes()
        await awaitSubscriber(environment.libraryStore.albumChanges)
        let created = try await environment.albums.createAlbum(
            name: "print",
            policy: AlbumPolicy(historyPolicy: .full, retentionDays: 30, protocolVersion: "2026-05-01")
        )
        var iterator = stream.makeAsyncIterator()
        #expect(await iterator.next() != nil)
        #expect(try await environment.albums.containerAlbum(created.id)?.name == "print")
        #expect(try await environment.albums.containerAlbums().contains { $0.id == created.id })
    }

    /// The designated default cannot be deleted while designated — the user must
    /// repoint first, so import always has a home.
    @Test("The default album refuses deletion")
    func defaultAlbumIsUndeletable() async throws {
        let environment = makeEnvironment()
        let albums = try await environment.albums.containerAlbums()
        guard let fallback = albums.first(where: { $0.isDefault }) else {
            Issue.record("no default album was designated")
            return
        }
        #expect(fallback.name == nil)
        #expect(!fallback.isDeletable)
        await #expect(throws: CapsuleError.self) {
            try await environment.albums.deleteAlbum(fallback.id)
        }
    }

    /// Membership changes are commits: the epoch has to move.
    @Test("Inviting a member bumps the album epoch")
    func invitingBumpsTheEpoch() async throws {
        let environment = makeEnvironment()
        let album = try await environment.albums.containerAlbums()[1]
        try await environment.albums.inviteMember(handle: "kai@capsule.example", role: .write, to: album.id)
        let updated = try await environment.albums.containerAlbum(album.id)
        #expect(updated?.epoch == album.epoch + 1)
        #expect(updated?.members.contains { $0.handle == "kai@capsule.example" } == true)
        #expect(updated?.isShared == true)
    }

    @Test("Adopting a drop produces a resolvable asset and empties the inbox")
    func adoptingADrop() async throws {
        let environment = makeEnvironment()
        let stream = environment.drops.changes()
        await awaitSubscriber(environment.sharingStore.dropChanges)
        let inbox = try await environment.drops.pendingDrops(offset: 0, limit: 10)
        #expect(!inbox.items.isEmpty)
        let album = try await environment.albums.containerAlbums()[1]
        let adopted = try await environment.drops.adopt(inbox.items[0].id, into: album.id)
        var iterator = stream.makeAsyncIterator()
        #expect(await iterator.next() != nil)
        #expect(try await environment.library.asset(for: adopted)?.albumID == album.id)
        let after = try await environment.drops.pendingDrops(offset: 0, limit: 10)
        #expect(after.items.count == inbox.items.count - 1)
    }

    /// A view-layer choice, not deletion: the asset leaves the default timeline
    /// and appears in Hidden, and comes back intact.
    @Test("Hiding moves an asset into the Hidden view and back")
    func hidingRoundTrips() async throws {
        let environment = makeEnvironment()
        let subject = try await environment.library.assets(matching: .default, offset: 0, limit: 1).items[0]
        let hiddenBefore = try await environment.library.assetCount(matching: .hidden)
        try await environment.organize.setHidden(true, for: [subject.id])
        #expect(try await environment.library.assetCount(matching: .hidden) == hiddenBefore + 1)
        #expect(try await environment.library.asset(for: subject.id)?.isDeleted == false)
        try await environment.organize.setHidden(false, for: [subject.id])
        #expect(try await environment.library.assetCount(matching: .hidden) == hiddenBefore)
    }

    /// An invalid predicate is a structural rejection, never a tolerated
    /// definition — a predicate that evaluated differently on two devices would
    /// show two different albums under one name.
    @Test("Smart albums save, evaluate, and reject invalid predicates")
    func smartAlbumEditing() async throws {
        let environment = makeEnvironment()
        let port = environment.smartAlbums
        let definition = SmartAlbumDefinition(
            smartAlbumID: SmartAlbumID("test-video"),
            displayName: Lww(),
            predicate: .term(Term(
                field: .mediaKind,
                operatorKind: .equalTo,
                operand: .enumerationValue(MediaKind.video.rawValue)
            ))
        )
        try await port.save(definition)
        #expect(try await port.definition(definition.id) != nil)
        let evaluated = try await port.evaluate(definition.id, offset: 0, limit: 50)
        #expect(evaluated.items.allSatisfy { $0.contentType.mediaKind == .video })

        await #expect(throws: PredicateValidationError.self) {
            _ = try await port.preview(
                .term(Term(field: .rating, operatorKind: .contains, operand: .stringSet(["x"]))),
                limit: 10
            )
        }

        try await port.delete(definition.id)
        #expect(try await port.definition(definition.id) == nil)
    }
}
