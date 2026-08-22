import CapsuleDiagnostics
import CapsuleFoundation
import CapsuleUI
import FeatureCollections
import FeatureSearch
import FeatureTimeline
import SwiftUI

/// The app's root tab shell: Library, Collections, Search, and Settings.
///
/// A compact `TabView` today; Phase 7 adds the `NavigationSplitView` layout for
/// regular-width (iPad) size classes. This view also hosts the app-wide
/// diagnostics hooks: low-memory recording, clean-shutdown tracking, and the
/// "crashed last launch?" prompt.
struct RootView: View {
    let environment: AppEnvironment

    @Environment(\.scenePhase) private var scenePhase
    @State private var crashReportOffered = false
    @State private var crashReport: DiagnosticsReport?

    var body: some View {
        TabView {
            Tab("ios.tab.library", systemImage: "photo.on.rectangle.angled") {
                TimelineRootView(
                    assetProvider: environment.assetProvider,
                    albumProvider: environment.albumProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader,
                    importer: environment.importer,
                    hiddenStore: environment.hiddenStore
                )
            }
            Tab("ios.tab.collections", systemImage: "rectangle.stack") {
                CollectionsRootView(
                    albumProvider: environment.albumProvider,
                    assetProvider: environment.assetProvider,
                    trashProvider: environment.trashProvider,
                    hiddenStore: environment.hiddenStore,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader
                )
            }
            Tab("ios.tab.search", systemImage: "magnifyingglass", role: .search) {
                SearchRootView(
                    assetProvider: environment.assetProvider,
                    albumProvider: environment.albumProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader
                )
            }
            Tab("ios.tab.settings", systemImage: "gearshape") {
                SettingsView(
                    consentStore: environment.consentStore,
                    diagnostics: environment.diagnostics
                )
            }
        }
        .tabViewStyle(.sidebarAdaptable)
        .capsuleTabBarMinimizeOnScroll()
        .onReceive(MemoryPressure.publisher) { _ in
            Diagnostics.shared.record(.memoryWarning)
            Task { await environment.thumbnails.flushCaches() }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                Task { await environment.diagnostics.noteEnteredBackground() }
            case .active:
                Task { await environment.diagnostics.noteBecameActive() }
            default:
                break
            }
        }
        .task {
            if await environment.diagnostics.shouldOfferCrashReport() {
                crashReportOffered = true
            }
        }
        .alert("ios.diagnostics.crash_alert.title", isPresented: $crashReportOffered) {
            Button("ios.diagnostics.crash_alert.send") {
                Task {
                    let bundle = await environment.diagnostics.makeReportBundle()
                    await environment.diagnostics.acknowledgeCrashReport()
                    if let data = try? bundle.jsonData() {
                        crashReport = DiagnosticsReport(data: data)
                    }
                }
            }
            Button("ios.common.not_now", role: .cancel) {
                Task { await environment.diagnostics.acknowledgeCrashReport() }
            }
        } message: {
            Text("ios.diagnostics.crash_alert.body")
        }
        .sheet(item: $crashReport) { DiagnosticsReportView(report: $0) }
    }
}
