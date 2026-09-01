import Foundation
import Testing

import CapsuleDomain

/// The CRDT value types: LWW with its superseded log, and the observed-remove
/// set (*Metadata — Collaborative Metadata*, *Surfacing Concurrent Edits*).
@Suite("CRDT registers converge and surface what they displaced")
struct CRDTTests {
    // MARK: LWW ordering

    @Test("a later timestamp wins and the loser is preserved")
    func laterWins() {
        let register = Lww<String>()
            .applying(Fixtures.stamped("first", offsetSeconds: 0))
            .applying(Fixtures.stamped("second", offsetSeconds: 10))

        #expect(register.value == "second")
        #expect(register.superseded.map(\.value) == ["first"])
    }

    @Test("a tied timestamp breaks on the larger device id")
    func tieBreaksOnDevice() {
        // Without the tiebreak, two devices writing in the same millisecond
        // converge to different states — the exact divergence the CRDT exists
        // to prevent.
        let register = Lww<String>()
            .applying(Fixtures.stamped("from A", offsetSeconds: 0, author: Fixtures.deviceA))
            .applying(Fixtures.stamped("from B", offsetSeconds: 0, author: Fixtures.deviceB))

        #expect(Fixtures.deviceB > Fixtures.deviceA)
        #expect(register.value == "from B")
        #expect(register.superseded.map(\.value) == ["from A"])
    }

    @Test("a late-arriving loser is still recorded so the user can restore it")
    func lateLoserIsRecorded() {
        // Arrival order must not decide what the user gets to see. A write that
        // loses on merge is still an edit somebody made.
        let register = Lww<String>()
            .applying(Fixtures.stamped("newer", offsetSeconds: 10))
            .applying(Fixtures.stamped("older", offsetSeconds: 0))

        #expect(register.value == "newer")
        #expect(register.superseded.map(\.value) == ["older"])
    }

    @Test("re-writing the same value does not pad the superseded log")
    func duplicateValueNotSuperseded() {
        let register = Lww<String>()
            .applying(Fixtures.stamped("same", offsetSeconds: 10))
            .applying(Fixtures.stamped("same", offsetSeconds: 0))

        #expect(register.value == "same")
        #expect(register.superseded.isEmpty)
    }

    @Test("the superseded log caps at sixteen, evicting the oldest")
    func supersededCap() {
        var register = Lww<String>().applying(Fixtures.stamped("current", offsetSeconds: 1000))
        for index in 0 ..< 30 {
            register = register.applying(Fixtures.stamped("loser \(index)", offsetSeconds: Int64(index)))
        }

        #expect(register.value == "current")
        #expect(register.superseded.count == Lww<String>.supersededCap)
        // Newest-superseded first, so the most recently displaced edit is the
        // one the user is most likely to want back.
        #expect(register.superseded.first?.value == "loser 29")
    }

    @Test("merge is order-independent")
    func mergeConverges() {
        let left = Lww<String>()
            .applying(Fixtures.stamped("a", offsetSeconds: 0, author: Fixtures.deviceA))
        let right = Lww<String>()
            .applying(Fixtures.stamped("b", offsetSeconds: 5, author: Fixtures.deviceB))

        let forward = left.merging(right)
        let backward = right.merging(left)

        #expect(forward.value == backward.value)
        #expect(forward.value == "b")
        #expect(Set(forward.superseded.map(\.value)) == Set(backward.superseded.map(\.value)))
    }

    @Test("merge is idempotent")
    func mergeIsIdempotent() {
        let register = Lww<String>()
            .applying(Fixtures.stamped("a", offsetSeconds: 0))
            .applying(Fixtures.stamped("b", offsetSeconds: 5))
        #expect(register.merging(register) == register)
    }

    @Test("a never-written register is distinct from one written to a default")
    func neverWrittenIsDistinct() {
        // The distinction is what wire-absent defaults depend on: `cull` absent
        // means never flagged, while `cull` stamped to neutral means somebody
        // un-flagged it, and the two must merge differently.
        let untouched = Lww<CullFlag>()
        let stamped = Lww<CullFlag>().applying(
            Stamped(value: .neutral, timestamp: Fixtures.epoch, author: Fixtures.deviceA)
        )
        #expect(!untouched.hasBeenWritten)
        #expect(stamped.hasBeenWritten)
        #expect(untouched.value == nil)
        #expect(stamped.value == .neutral)
    }

    // MARK: OR-set

    @Test("an add appears in the live value and a remove tombstones it")
    func addAndRemove() throws {
        let addID = AddID(deviceID: Fixtures.deviceA, counter: 1)
        let withTag = OrSet<String>().adding("beach", addID: addID)
        #expect(withTag.value == ["beach"])

        let removed = try withTag.removing(addID)
        #expect(removed.value.isEmpty)
    }

    @Test("removing an add this replica never observed is rejected, not a no-op")
    func unobservedRemoveRejected() {
        // Tolerating it is the "remove an element you never added" attack: a
        // peer could tombstone a tag it never saw, deleting a user's data on
        // every replica that accepted the operation.
        let set = OrSet<String>().adding("beach", addID: AddID(deviceID: Fixtures.deviceA, counter: 1))
        let foreign = AddID(deviceID: Fixtures.deviceB, counter: 99)

        #expect(throws: UnobservedRemove(addID: foreign)) {
            _ = try set.removing(foreign)
        }
        #expect(!set.hasObserved(foreign))
    }

    @Test("re-adding a removed element is a new element, not a resurrection")
    func reAddIsNew() throws {
        let first = AddID(deviceID: Fixtures.deviceA, counter: 1)
        let second = AddID(deviceID: Fixtures.deviceA, counter: 2)

        let set = try OrSet<String>()
            .adding("beach", addID: first)
            .removing(first)
            .adding("beach", addID: second)

        #expect(set.value == ["beach"])
        // Replaying the original remove must not delete the new add.
        let replayed = try set.removing(first)
        #expect(replayed.value == ["beach"])
    }

    @Test("merge is commutative and idempotent")
    func orSetMergeConverges() throws {
        let addA = AddID(deviceID: Fixtures.deviceA, counter: 1)
        let addB = AddID(deviceID: Fixtures.deviceB, counter: 1)

        let left = OrSet<String>().adding("sunset", addID: addA)
        let right = try OrSet<String>()
            .adding("sunset", addID: addA)
            .adding("beach", addID: addB)
            .removing(addA)

        let forward = left.merging(right)
        let backward = right.merging(left)

        #expect(forward.value == backward.value)
        #expect(forward.value == ["beach"])
        #expect(forward.merging(forward) == forward)
    }

    @Test("entries are ordered by add id so a UI list is stable across reads")
    func entriesAreOrdered() {
        let set = OrSet<String>()
            .adding("second", addID: AddID(deviceID: Fixtures.deviceA, counter: 2))
            .adding("first", addID: AddID(deviceID: Fixtures.deviceA, counter: 1))
        #expect(set.entries.map(\.element) == ["first", "second"])
    }

    @Test("AI tags keep their model slot through the set")
    func aiTagsCarryModelSlot() {
        // A term over `tags_ai` names the slot it queries, so the slot must
        // survive into the set — comparing tags across model versions is
        // forbidden.
        let tag = AiTag(tag: "dog", modelID: "clip", modelVersion: "1.2")
        let set = OrSet<AiTag>().adding(tag, addID: AddID(deviceID: Fixtures.deviceA, counter: 1))
        #expect(set.value.first?.modelSlot == ModelSlot(modelID: "clip", modelVersion: "1.2"))
    }
}
