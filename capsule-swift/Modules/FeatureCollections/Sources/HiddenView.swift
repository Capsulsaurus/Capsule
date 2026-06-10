import AssetKit
import CapsuleUI
import ImagePipeline
import LocalAuthentication
import SwiftUI

/// The Hidden album — photos hidden via the Library's select → Hide action,
/// behind a Face ID / passcode gate. Tapping a tile offers to unhide it.
///
/// Hidden ids live in a Swift ``HiddenStore`` overlay (symmetric across PhotoKit
/// and managed sources); the assets are resolved back through the provider.
struct HiddenView: View {
    @State private var unlocked = false
    @State private var assets: [Asset] = []
    @State private var isLoading = false
    @State private var unhideTarget: Asset?
    private let assetProvider: any AssetProvider
    private let hiddenStore: HiddenStore
    private let thumbnails: any ThumbnailProvider

    init(
        assetProvider: any AssetProvider,
        hiddenStore: HiddenStore,
        thumbnails: any ThumbnailProvider
    ) {
        self.assetProvider = assetProvider
        self.hiddenStore = hiddenStore
        self.thumbnails = thumbnails
    }

    var body: some View {
        Group {
            if !unlocked {
                lockedView
            } else if isLoading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if assets.isEmpty {
                ContentUnavailableView(
                    "Nothing Hidden",
                    systemImage: "eye.slash",
                    description: Text("Hide photos from the Library with Select → Hide.")
                )
            } else {
                grid
            }
        }
        .navigationTitle("Hidden")
        .navigationBarTitleDisplayMode(.inline)
        .task { await authenticate() }
        .confirmationDialog(
            "Hidden Photo",
            isPresented: unhidePresented,
            titleVisibility: .visible,
            presenting: unhideTarget
        ) { asset in
            Button("Unhide") { Task { await unhide(asset) } }
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
            Label("Hidden", systemImage: "lock.fill")
        } description: {
            Text("Authenticate to view your hidden photos.")
        } actions: {
            Button("Unlock") { Task { await authenticate() } }
        }
    }

    private var unhidePresented: Binding<Bool> {
        Binding(get: { unhideTarget != nil }, set: { if !$0 { unhideTarget = nil } })
    }

    private func authenticate() async {
        let context = LAContext()
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            // No biometrics/passcode enrolled (e.g. a fresh simulator): allow in.
            unlocked = true
            await loadHidden()
            return
        }
        let success = (try? await context.evaluatePolicy(
            .deviceOwnerAuthentication, localizedReason: "View your hidden photos"
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
