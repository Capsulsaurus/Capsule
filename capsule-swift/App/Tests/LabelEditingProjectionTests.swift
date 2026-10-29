import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import FeatureViewer
import Testing

@testable import Capsule

/// The label editor's ports, as the composition root hands them to the
/// environment: the same label port every screen reads, and the stack the
/// library records for an asset.
@MainActor
@Suite("Label editing projection")
struct LabelEditingProjectionTests {
    @Test("an asset's stack is the one the library records for it")
    func stackLookupFollowsTheLibrary() async throws {
        let environment = AppEnvironment(mock: MockEnvironment(scenario: .healthy))
        let editing = environment.labelEditing
        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 64)
        #expect(!page.items.isEmpty)
        for asset in page.items {
            let found = await editing.stackOf(asset.id)
            #expect(found == asset.stackMembership?.stackID)
        }
    }

    @Test("a label written through the projection is the one the label port holds")
    func labelsWriteThroughTheSharedPort() async throws {
        let environment = AppEnvironment(mock: MockEnvironment(scenario: .healthy))
        let album = try #require(try await environment.albums.containerAlbums().first)
        let answer = try await environment.labelEditing.port.addLabel(" Trips ", to: .album(album.id))
        #expect(answer == ["Trips"])
        #expect(try await environment.labels.labels(on: .album(album.id)) == ["Trips"])
    }

    @Test("the transfer view's stream is the mock lane's recorded one")
    func transferEventsForwardTheMock() async {
        let mock = MockEnvironment(scenario: .healthy)
        let environment = AppEnvironment(mock: mock)
        var forwarded: [TransferFeedItem] = []
        for await item in environment.transferEvents.events() {
            forwarded.append(item)
        }
        #expect(forwarded == mock.transferEventFeed.script)
    }
}
