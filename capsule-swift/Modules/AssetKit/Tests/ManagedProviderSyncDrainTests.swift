import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleTestSupport
import ManagedStore

/// A sync that drains the feed and lists newly indexed assets releases held
/// purges, deletes and restores, so it runs the walk a launch runs; one that
/// lists nothing new walks nothing.
@Suite("ManagedProvider redelivers held records after a sync drain")
struct ManagedProviderSyncDrainTests {
    private func makeProvider(delivery: JournalingDelivery?) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(
                layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
                fileStore: MockFileStore(),
                catalog: MockCatalog()
            ),
            authGate: MockLocalAuthGate(),
            purgeDelivery: delivery,
            lifecycleDelivery: delivery
        )
    }

    /// One walk over a delivery holding the kept purge `p` and the held asset `h`.
    private static let walk = ["list purges", "purge p", "list held", "deliver h"]

    @Test("a drain that lists a newly indexed asset runs the purge walk, then the lifecycle walk")
    func newlyIndexedAssetRunsBothWalks() async {
        let journal = Journal()
        let delivery = JournalingDelivery(journal: journal, purged: ["p"], held: ["h"])
        let provider = makeProvider(delivery: delivery)

        await provider.syncDrained(newlyIndexed: ["fresh"])

        #expect(await journal.entries == Self.walk)
    }

    @Test("a drain that lists nothing new walks nothing")
    func nothingNewWalksNothing() async {
        let journal = Journal()
        let delivery = JournalingDelivery(journal: journal, purged: ["p"], held: ["h"])
        let provider = makeProvider(delivery: delivery)

        await provider.syncDrained(newlyIndexed: [])

        #expect(await journal.entries.isEmpty)
    }

    @Test("with no delivery port a drain that lists new assets has nothing to send")
    func noPortNothingToSend() async {
        let provider = makeProvider(delivery: nil)

        await provider.syncDrained(newlyIndexed: ["fresh"])
        await provider.redeliverHeldRecords()
    }

    @Test("a drain that lands while the launch walk runs queues one more walk after it, never beside it")
    func drainDuringAWalkQueuesBehindIt() async {
        let journal = Journal()
        let gate = Gate()
        let delivery = JournalingDelivery(journal: journal, purged: ["p"], held: ["h"], firstListGate: gate)
        let provider = makeProvider(delivery: delivery)

        let launch = Task { await provider.redeliverHeldRecords() }
        await gate.waitUntilEntered()
        let sync = Task { await provider.syncDrained(newlyIndexed: ["fresh"]) }
        for _ in 0 ..< 200 {
            await Task.yield()
        }

        #expect(await journal.entries == ["list purges"], "the sync's walk must wait for the launch walk")
        await gate.open()
        await launch.value
        await sync.value

        #expect(await journal.entries == Self.walk + Self.walk)
    }
}

/// What a delivery saw, in the order it saw it.
private actor Journal {
    private(set) var entries: [String] = []

    func record(_ entry: String) {
        entries.append(entry)
    }
}

/// A gate the first purge listing waits at until the test opens it, so a
/// walk can be held mid-flight while another is asked for.
private actor Gate {
    private var entered = false
    private var isOpen = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var passWaiters: [CheckedContinuation<Void, Never>] = []

    /// Mark the gate entered, then wait until it is open.
    func pass() async {
        entered = true
        for waiter in entryWaiters {
            waiter.resume()
        }
        entryWaiters = []
        guard !isOpen else { return }
        await withCheckedContinuation { passWaiters.append($0) }
    }

    /// Wait until something has reached ``pass()``.
    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { entryWaiters.append($0) }
    }

    /// Let everything waiting at ``pass()``, and everything that reaches it
    /// later, through.
    func open() {
        isOpen = true
        for waiter in passWaiters {
            waiter.resume()
        }
        passWaiters = []
    }
}

/// A ``PurgeDelivering`` and ``LifecycleDelivering`` that journals each
/// listing and delivery and holds every one (``PurgeDelivery/held``,
/// ``LifecycleDelivery/held``), so no walk settles anything and each walks
/// the same assets. The first purge listing waits at `firstListGate`.
private actor JournalingDelivery: PurgeDelivering, LifecycleDelivering {
    private let journal: Journal
    private let purged: [String]
    private let held: [String]
    private var firstListGate: Gate?

    init(journal: Journal, purged: [String], held: [String], firstListGate: Gate? = nil) {
        self.journal = journal
        self.purged = purged
        self.held = held
        self.firstListGate = firstListGate
    }

    func purgedAssetIDs() async -> [String] {
        await journal.record("list purges")
        if let gate = firstListGate {
            firstListGate = nil
            await gate.pass()
        }
        return purged
    }

    func deliverPurge(assetID: String) async -> PurgeDelivery {
        await journal.record("purge \(assetID)")
        return .held
    }

    func heldAssetIDs() async -> [String] {
        await journal.record("list held")
        return held
    }

    func deliverLifecycle(assetID: String) async -> LifecycleDelivery {
        await journal.record("deliver \(assetID)")
        return .held
    }
}
