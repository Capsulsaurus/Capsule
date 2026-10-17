import Foundation
import Testing

import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import FeatureSharing

// MARK: - ShareLinkListViewModelTests

@Suite("ShareLinkListViewModel")
@MainActor
struct ShareLinkListViewModelTests {
    private func makeModel(_ environment: MockEnvironment) -> ShareLinkListViewModel {
        ShareLinkListViewModel(
            share: environment.sharing,
            connectivity: SharingConnectivity(sync: environment.sync),
            now: { MockClock.reference.now }
        )
    }

    @Test("live links list as active and lapsed ones stay on record")
    func splitsByLiveness() async {
        let model = makeModel(MockEnvironment(scenario: .healthy))

        await model.load()

        #expect(model.phase == .ready)
        #expect(!model.active.isEmpty)
        // Revoked and expired links are kept: a link already opened cannot be
        // un-shared, so the record is the only account of what was handed out.
        #expect(!model.inactive.isEmpty)
        #expect(model.inactive.contains { $0.lapse == .revoked })
        #expect(model.inactive.contains { $0.lapse == .expired })
    }

    @Test("revoking removes the link from the active list")
    func revocationLeavesActiveList() async {
        let model = makeModel(MockEnvironment(scenario: .healthy))
        await model.load()
        let target = try? #require(model.active.first)
        guard let target else { return }
        let activeCountBefore = model.active.count

        await model.revoke(target)

        #expect(!model.active.contains { $0.id == target.id })
        #expect(model.active.count == activeCountBefore - 1)
        // Moved, not deleted.
        #expect(model.inactive.contains { $0.id == target.id })
        #expect(model.inactive.first { $0.id == target.id }?.lapse == .revoked)
    }

    @Test("a revoke confirmation is cleared once it is acted on")
    func revocationClearsPendingConfirmation() async {
        let model = makeModel(MockEnvironment(scenario: .healthy))
        await model.load()
        let target = try? #require(model.active.first)
        guard let target else { return }
        model.pendingRevocation = target

        await model.revoke(target)

        #expect(model.pendingRevocation == nil)
    }

    @Test("a row never carries the fragment secret")
    func rowOmitsSecret() async {
        let environment = MockEnvironment(scenario: .healthy)
        let links = try? await environment.sharing.links()
        let link = try? #require(links?.first)
        guard let link else { return }

        let row = ShareLinkRow(link: link, now: MockClock.reference.now)

        // The projection is what the list renders and what a crash reporter
        // would capture; the secret must not be reachable from it.
        #expect(!String(describing: row).contains(link.secret))
    }
}

// MARK: - ShareLinkComposerViewModelTests

@Suite("ShareLinkComposerViewModel")
@MainActor
struct ShareLinkComposerViewModelTests {
    private func makeModel(scope: ShareScope) -> ShareLinkComposerViewModel {
        let environment = MockEnvironment(scenario: .healthy)
        return ShareLinkComposerViewModel(
            scope: scope,
            share: environment.sharing,
            homeServer: "capsule.example",
            connectivity: SharingConnectivity(sync: environment.sync),
            now: MockClock.reference.now.date
        )
    }

    @Test("the issued URL carries the secret in the fragment, not the path")
    func secretStaysInFragment() async {
        let model = makeModel(scope: .album(MockIdentifiers.albumID(seed: 0x0C0F_FEE0_1234_5678, ordinal: 1)))

        await model.createLink()

        let url = try? #require(model.shareURL)
        let issued = try? #require(model.issued)
        guard let url, let issued else { return }
        // The whole share design rests on this: a browser never sends a
        // fragment to the server.
        #expect(url.fragment() == issued.secret)
        #expect(!url.path().contains(issued.secret))
    }

    @Test("resetting drops the issued link and its secret")
    func resetDropsSecret() async {
        let model = makeModel(scope: .asset(.managed(uuid: "asset-1")))
        await model.createLink()
        #expect(model.issued != nil)

        model.reset()

        #expect(model.issued == nil)
        #expect(model.shareURL == nil)
    }

    @Test("a passphrase-protected link cannot be created with a blank passphrase")
    func blankPassphraseBlocksSubmit() {
        let model = makeModel(scope: .asset(.managed(uuid: "asset-1")))
        model.passphraseEnabled = true
        model.passphrase = "   "

        #expect(!model.canSubmit)

        model.passphrase = "correct horse"
        #expect(model.canSubmit)
    }

    @Test("an album scope is flagged as album-wide")
    func albumScopeIsFlagged() {
        let albumModel = makeModel(scope: .album(.managed(uuid: "album-1")))
        let assetModel = makeModel(scope: .asset(.managed(uuid: "asset-1")))

        // Album scope hands over the AMK for every epoch the history policy
        // covers — categorically more than one file key.
        #expect(albumModel.isAlbumWide)
        #expect(!assetModel.isAlbumWide)
    }
}
