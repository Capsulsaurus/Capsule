import AssetKit
import CapsulePorts
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// The Hidden album — photos hidden via the Library's select → Hide action,
/// behind a Face ID / passcode gate. Tapping a tile offers to unhide it.
///
/// Hidden ids live in a Swift ``HiddenStore`` overlay (symmetric across PhotoKit
/// and managed sources); the assets are resolved back through the provider.
public struct HiddenView: View {
    @State private var unlocked = false
    @State private var assets: [Asset] = []
    @State private var isLoading = false
    @State private var unhideTarget: Asset?
    private let assetProvider: any AssetProvider
    private let hiddenStore: HiddenStore
    private let thumbnails: any ThumbnailProvider
    private let authenticator: any LocalAuthenticator

    public init(
        assetProvider: any AssetProvider,
        hiddenStore: HiddenStore,
        thumbnails: any ThumbnailProvider,
        authenticator: any LocalAuthenticator
    ) {
        self.assetProvider = assetProvider
        self.hiddenStore = hiddenStore
        self.thumbnails = thumbnails
        self.authenticator = authenticator
    }

    public var body: some View {
        Group {
            if !unlocked {
                lockedView
            } else if isLoading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if assets.isEmpty {
                ContentUnavailableView(
                    "ios.hidden.empty.title",
                    systemImage: "eye.slash",
                    description: Text("ios.hidden.empty.description")
                )
            } else {
                grid
            }
        }
        .navigationTitle("ios.hidden.title")
        .capsuleNavigationBarInline()
        .task { await authenticate() }
        .confirmationDialog(
            "ios.hidden.confirm.title",
            isPresented: unhidePresented,
            titleVisibility: .visible,
            presenting: unhideTarget
        ) { asset in
            Button("ios.hidden.unhide") { Task { await unhide(asset) } }
        }
    }

    private var grid: some View {
        PhotoGridView(
            sections: [PhotoGridSection(id: "hidden", title: "", assets: assets)],
            columnCount: 5,
            thumbnails: thumbnails,
            showsSectionHeaders: false,
            onSelect: { unhideTarget = $0 }
        )
        .ignoresSafeArea(edges: .bottom)
    }

    private var lockedView: some View {
        ContentUnavailableView {
            Label("ios.hidden.title", systemImage: "lock.fill")
        } description: {
            Text("ios.hidden.locked.description")
        } actions: {
            Button("ios.hidden.unlock") { Task { await authenticate() } }
        }
    }

    private var unhidePresented: Binding<Bool> {
        Binding(get: { unhideTarget != nil }, set: { if !$0 { unhideTarget = nil } })
    }

    /// Run the gate through the injected authenticator.
    ///
    /// Through the seam rather than `LAContext` directly. This screen used to
    /// build its own context, which made it a second implementation of a
    /// ceremony `LocalAuthGate` already owns — and meant the mocked app, which
    /// composes no system services anywhere else, opened a system passcode
    /// sheet the moment someone tapped Hidden.
    ///
    /// A device with no credential at all opens: *Local Gallery — SR1* wants
    /// the gate reported as unavailable rather than the view sealed shut, and
    /// `SettingsSecurityView` is where that is said out loud.
    private func authenticate() async {
        if await authenticator.availableMethod() == .unavailable {
            unlocked = true
            await loadHidden()
            return
        }
        let success = await (try? authenticator.authenticate(
            reasonKey: "ios.hidden.auth.reason"
        )) ?? false
        unlocked = success
        if success { await loadHidden() }
    }

    private func loadHidden() async {
        isLoading = true
        let ids = await hiddenStore.hiddenIDs()
        var resolved: [Asset] = []
        for id in ids {
            if let asset = try? await assetProvider.asset(for: id) {
                resolved.append(asset)
            }
        }
        assets = resolved.sorted { $0.captureDate > $1.captureDate }
        isLoading = false
    }

    private func unhide(_ asset: Asset) async {
        await hiddenStore.setHidden(false, for: [asset.id])
        unhideTarget = nil
        await loadHidden()
    }
}
