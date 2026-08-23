import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import FeatureSettings
import Foundation
import SwiftUI
import Testing

@testable import Capsule

/// What every `Route` actually presents.
///
/// The point of these tests is not that `RouteDestination` compiles — the
/// exhaustive `switch` already guarantees that every case has *a* destination.
/// It is that the destination is a **screen** rather than a placeholder, for
/// every route a user can actually get to. A route that quietly falls back to
/// ``RouteScaffold`` looks identical to a route nobody has wired: both navigate,
/// both render, and neither shows the feature that exists three modules away.
@MainActor
@Suite("Route destinations")
struct RouteDestinationTests {
    // MARK: - The declared gaps

    /// Every route that still resolves to ``RouteScaffold``, with the reason.
    ///
    /// This is a **contract, not a record**. A route may sit here only
    /// deliberately, and the moment its screen lands the line is deleted — which
    /// is the whole mechanism: the list shrinks because a test fails when it is
    /// out of date, rather than because somebody remembers to prune it.
    ///
    /// Adding a line is therefore a decision, and reviewable as one.
    static let unbuilt: [UnbuiltRoute] = [
        // These two used to resolve to the album index, which scored as "a
        // screen" because it is not a scaffold. Showing the wrong screen is a
        // worse answer than admitting there is none.
        UnbuiltRoute(.memories, "the generated memories shelf is not built"),
        UnbuiltRoute(.duplicates, "the duplicate-review surface is not built"),
        UnbuiltRoute(.viewer(assetID, context: .library), "the full-screen viewer is not built"),
        UnbuiltRoute(.culling(.library), "the keyboard cull pass is not built"),
        UnbuiltRoute(.albumMembers(albumID), "the participant list is not built"),
        UnbuiltRoute(.albumPolicy(albumID), "the sharing and retention editor is not built"),
        UnbuiltRoute(.smartAlbum(smartAlbumID), "smart-album results have no screen of their own"),
        UnbuiltRoute(.smartAlbumEditor(nil), "the predicate editor is not built"),
        UnbuiltRoute(.smartAlbumEditor(smartAlbumID), "the predicate editor is not built"),
        UnbuiltRoute(.people, "face clustering has no index screen"),
        UnbuiltRoute(.person(personID), "face clustering has no cluster screen"),
        UnbuiltRoute(.importSession(importID), "one run's candidates and outcome have no screen"),
        UnbuiltRoute(.shareDetail(shareID), "a link is inspected and revoked from the list"),
        UnbuiltRoute(.linkRedemption(.share, opaqueID: "abc123"), "redeeming an inbound link is not built"),
        UnbuiltRoute(.linkRedemption(.guestUpload, opaqueID: "def456"), "redeeming an inbound link is not built"),
        UnbuiltRoute(.onboarding(.photoAccess), "the access rationale screen is not built"),
        UnbuiltRoute(.onboarding(.finish), "the hand-off into the library is not built"),
        UnbuiltRoute(.settings(.federation), "peer budgets and breakers have no screen"),
        UnbuiltRoute(.settings(.advanced), "the escape hatches have no screen"),
        UnbuiltRoute(.settings(.about), "version and acknowledgements have no screen"),
    ]

    static let unbuiltRoutes = Set(unbuilt.map(\.route))

    // MARK: - Tests

    /// Nothing the user can navigate to is a placeholder unless it is declared.
    ///
    /// Reachable means what a *user* can do, not what the enum can express:
    /// every sidebar row's landing screen, every URL the deep-link parser
    /// accepts, and every menu command the router consumes.
    @Test("every reachable route presents a real screen unless it is a declared gap")
    func reachableRoutesPresentRealScreens() {
        let environment = AppEnvironment()
        for route in Self.reachableRoutes {
            let isScaffold = Self.presentsScaffold(route, in: environment)
            #expect(
                isScaffold == Self.unbuiltRoutes.contains(route),
                """
                \(route) is \(isScaffold ? "a scaffold" : "a screen") but the \
                declared list says otherwise. Either wire it, or add it to \
                `unbuilt` with the reason.
                """
            )
        }
    }

    /// The declared list agrees with the switch, for every route — including the
    /// ones nothing can currently navigate to.
    ///
    /// Without this the list could describe a world that stopped being true: a
    /// screen lands, the switch is updated, and the gap stays declared forever.
    @Test("the declared gaps are exactly what the switch scaffolds")
    func declaredGapsMatchTheSwitch() {
        let environment = AppEnvironment()
        for route in Self.census {
            #expect(
                Self.presentsScaffold(route, in: environment) == Self.unbuiltRoutes.contains(route),
                "\(route) disagrees with the declared not-built list"
            )
        }
    }

    /// The settings index is built from the catalog, so the catalog has to cover
    /// every screen. Asserted here rather than trusted, because an unfiled
    /// section is a screen that exists and cannot be opened.
    @Test("every settings section is filed in the root catalog")
    func settingsCatalogIsComplete() {
        #expect(SettingsRootCatalog.coversEverySection)
    }
}

// MARK: - Reachability

extension RouteDestinationTests {
    /// Every route a user can arrive at without the app pushing one itself.
    static var reachableRoutes: [Route] {
        sidebarRoutes + deepLinkRoutes + menuCommandRoutes
    }

    /// What each sidebar row (and each iPhone tab) lands on.
    static var sidebarRoutes: [Route] {
        SidebarItem.allCases.map(\.rootRoute)
    }

    /// What the deep-link parser can produce, driven through the real parser so
    /// a change to the URL grammar changes this set too.
    static var deepLinkRoutes: [Route] {
        [
            "capsule://album/0192f0c0-2222-7000-8000-000000000002",
            "capsule://asset/0192f0c0-1111-7000-8000-000000000001",
            "capsule://search?q=sunset",
            "https://photos.example.org/s/abc123#secret1",
            "https://photos.example.org/u/def456#secret2",
        ]
        .compactMap(URL.init(string:))
        .compactMap(DeepLink.route(for:))
    }

    /// Where each menu command leaves the router.
    static var menuCommandRoutes: [Route] {
        let router = Router(shell: .split)
        return NavigationCommand.all.compactMap { command in
            router.perform(command.action) ? router.topRoute : nil
        }
    }
}

// MARK: - Scaffold detection

extension RouteDestinationTests {
    /// Whether the route's destination is a placeholder.
    ///
    /// Read off the rendered view rather than off a second hand-written table:
    /// the `switch` in ``RouteDestination`` stays the only source of truth, so
    /// this cannot drift from it the way a parallel predicate would.
    static func presentsScaffold(_ route: Route, in environment: AppEnvironment) -> Bool {
        contains(RouteScaffold.self, in: RouteDestination(route: route, environment: environment).body)
    }

    /// Walk a `ViewBuilder` result looking for one view type.
    ///
    /// `switch` in a view body compiles to nested `_ConditionalContent`, which
    /// stores only the branch that was taken — so reflecting the value tells us
    /// which branch that was. The walk descends through views and through the
    /// plain wrappers a builder puts between them (the conditional's own storage
    /// enum, optionals, tuples) and stops everywhere else, which keeps it inside
    /// the view tree rather than descending into the ports a screen was handed.
    private static func contains<Target: View>(
        _ type: Target.Type,
        in value: Any,
        depth: Int = 0
    ) -> Bool {
        if value is Target { return true }
        guard depth < maximumViewDepth else { return false }
        let mirror = Mirror(reflecting: value)
        guard value is any View || isBuilderWrapper(mirror) else { return false }
        return mirror.children.contains { child in
            contains(type, in: child.value, depth: depth + 1)
        }
    }

    /// The plumbing a result builder puts between one view and the next.
    ///
    /// `_ConditionalContent` holds its branch in a private `Storage` enum, and
    /// that enum is not itself a view — so a walk that only descends through
    /// views stops one step short of every branch it was looking for.
    private static func isBuilderWrapper(_ mirror: Mirror) -> Bool {
        switch mirror.displayStyle {
        case .enum, .optional, .tuple: true
        default: false
        }
    }

    /// Deep enough for the nested `_ConditionalContent` a thirty-case switch
    /// builds, shallow enough that a recursive view type cannot hang the suite.
    private static let maximumViewDepth = 48
}

// MARK: - Fixtures

/// A route with no screen yet, and the reason it has none.
struct UnbuiltRoute: Sendable {
    let route: Route
    /// Why it is a placeholder. Present so the list reads as a set of decisions
    /// rather than a set of omissions.
    let reason: String

    init(_ route: Route, _ reason: String) {
        self.route = route
        self.reason = reason
    }
}

extension RouteDestinationTests {
    static let assetID = AssetID.managed(uuid: "0192f0c0-1111-7000-8000-000000000001")
    static let albumID = AlbumID.managed(uuid: "0192f0c0-2222-7000-8000-000000000002")
    static let smartAlbumID = SmartAlbumID("0192f0c0-3333-7000-8000-000000000003")
    static let personID = PersonID("0192f0c0-4444-7000-8000-000000000004")
    static let importID = ImportID("0192f0c0-5555-7000-8000-000000000005")
    static let quarantineID = QuarantineID("0192f0c0-6666-7000-8000-000000000006")
    static let shareID = ShareID("0192f0c0-7777-7000-8000-000000000007")
    static let dropID = DropID("0192f0c0-8888-7000-8000-000000000008")

    static let region = MapRegion(
        minimumLatitude: 43.6,
        maximumLatitude: 43.8,
        minimumLongitude: -79.5,
        maximumLongitude: -79.2
    )

    /// One sample of every `Route` case — and every *payload* that selects a
    /// different destination, which is why all eighteen settings sections and
    /// all eight onboarding steps appear rather than one of each.
    ///
    /// > Important: adding a case to `Route` means adding a row here.
    static var census: [Route] {
        [
            .timeline(.all), .timeline(.years),
            .memories, .duplicates, .trash, .hidden,
            .viewer(assetID, context: .library),
            .culling(.library),
            .browse,
            .albums, .album(albumID), .albumMembers(albumID), .albumPolicy(albumID),
            .smartAlbum(smartAlbumID), .smartAlbumEditor(nil), .smartAlbumEditor(smartAlbumID),
            .people, .person(personID),
            .places, .place(region),
            .search(.all, text: "sunset"),
            .transferCenter, .uploadDetail(assetID), .custodyReceipt(assetID),
            .imports, .importSession(importID),
            .quarantine, .quarantineItem(quarantineID),
            .shares, .shareDetail(shareID),
            .drops, .drop(dropID),
            .linkRedemption(.share, opaqueID: "abc123"),
            .linkRedemption(.guestUpload, opaqueID: "def456"),
            .devices, .peers, .federation,
            .quota, .storage, .maintenance(nil), .maintenance(.deepContentValidation),
        ]
            + SettingsSection.allCases.map(Route.settings)
            + OnboardingStep.allCases.map(Route.onboarding)
    }
}
