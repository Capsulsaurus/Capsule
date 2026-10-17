import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Scale

/// The claim this module exists to make good on: **library size is a parameter,
/// not a memory cost**.
///
/// `TimelineLayout` in `CapsuleUI` is tested against 3 650 sections and 250 000
/// items. If the mock materialized an array, the one thing most worth proving
/// about the UI could not be proven — so these tests assert the shape of the
/// work, not just the answers: construction is cheap, the aggregate is read off
/// the day boundaries, and a page request returns a page rather than a library.
@Suite("A 250 000-asset library is free to construct")
struct MockScaleTests {
    /// Deliberately loose. The point is the *order of magnitude* — a
    /// materializing implementation is hundreds of times over these, and a
    /// tight bound would only make the suite flaky on a busy machine.
    private static let constructionBudget = Duration.seconds(1)
    private static let queryBudget = Duration.seconds(2)

    @Test("The huge library constructs without materializing")
    func hugeLibraryConstructsQuickly() {
        let clock = ContinuousClock()
        var library: MockLibrary?
        let elapsed = clock.measure {
            library = MockLibrary(profile: MockLibraryProfile(
                assetCount: 250000,
                spanDays: 3650,
                newestDayNumber: MockClock.reference.todayDayNumber
            ))
        }
        #expect(library?.assetCount == 250000)
        #expect(elapsed < Self.constructionBudget)
    }

    @Test("The whole environment for .hugeLibrary builds quickly")
    func hugeEnvironmentBuildsQuickly() async throws {
        let clock = ContinuousClock()
        var environment: MockEnvironment?
        let elapsed = clock.measure {
            environment = MockEnvironment(scenario: .hugeLibrary)
        }
        #expect(elapsed < Self.constructionBudget)
        let count = try await environment?.library.assetCount(matching: .default)
        #expect(count == 250000)
    }

    /// The aggregate is O(days), so it stays cheap at ten years of photographs —
    /// and its total is exactly the library, with no drift to reconcile.
    @Test("Day counts over 250 000 assets are cheap and exact")
    func hugeDayCountsAreCheapAndExact() async throws {
        let environment = MockEnvironment(scenario: .hugeLibrary)
        let clock = ContinuousClock()
        var counts: [DayCount] = []
        let elapsed = clock.measure {
            counts = environment.libraryStore.library.unfilteredDayCounts()
        }
        #expect(elapsed < Self.queryBudget)
        #expect(counts.totalCount == 250000)
        #expect(counts.count > 3000)
        #expect(counts.allSatisfy { $0.count >= 1 })
    }

    /// A page request returns **only the page**. This is the assertion that
    /// would fail against a mock that built an array and sliced it.
    @Test("A page request returns only the page")
    func pagingReturnsOnlyTheWindow() async throws {
        let environment = MockEnvironment(scenario: .hugeLibrary)
        let clock = ContinuousClock()
        var page: Page<LibraryAsset>?
        let elapsed = clock.measure {
            // Measured synchronously against the engine so the timing is the
            // derivation's, not the actor hop's.
            page = MockQueryEngine(
                library: environment.libraryStore.library,
                overlay: MockOverlay(),
                now: environment.configuration.clock.now
            ).page(matching: .default, offset: 180000, limit: 200)
        }
        #expect(page?.items.count == 200)
        #expect(page?.totalCount == 250000)
        #expect(elapsed < Self.queryBudget)
    }

    /// A deep offset must be a subscript, not a scan, for the unfiltered
    /// timeline — which is what the day boundary fast path buys.
    @Test("A deep offset costs the same as a shallow one")
    func deepOffsetsAreNotAScan() async throws {
        let environment = MockEnvironment(scenario: .hugeLibrary)
        let engine = MockQueryEngine(
            library: environment.libraryStore.library,
            overlay: MockOverlay(),
            now: environment.configuration.clock.now
        )
        let clock = ContinuousClock()
        let shallow = clock.measure { _ = engine.page(matching: .default, offset: 0, limit: 200) }
        let deep = clock.measure { _ = engine.page(matching: .default, offset: 249000, limit: 200) }
        // Ten times the shallow cost, plus a floor so a sub-millisecond
        // measurement cannot make the ratio meaningless.
        #expect(deep < shallow * 10 + .milliseconds(50))
    }

    @Test("Assets at the far end of a huge library resolve by identifier")
    func farAssetsResolveByIdentifier() async throws {
        let environment = MockEnvironment(scenario: .hugeLibrary)
        let identifier = environment.libraryStore.library.identifier(at: 249999)
        let asset = try await environment.library.asset(for: identifier)
        #expect(asset?.id == identifier)
    }
}
