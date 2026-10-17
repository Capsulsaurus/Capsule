import CapsuleDiagnostics
import CapsuleFoundation
import CapsuleNavigation
import CapsuleUI
import SwiftUI

/// The app's root, and the only place that decides which shell the user sees.
///
/// One `Router` drives all three shells, so a route pushed from a menu command,
/// a deep link, or a tap inside a screen travels the same path regardless of
/// where it came from. What differs is only the *presentation*:
///
/// - **Compact** (iPhone, and iPad in a narrow split) — a `TabView` over the
///   four promoted sections, each owning its own `NavigationStack`.
/// - **Regular** (iPad full width, Mac) — a `NavigationSplitView` whose sidebar
///   lists all nineteen sections grouped by concern.
///
/// The choice is made from the live horizontal size class rather than from the
/// platform, because an iPad in Slide Over is genuinely compact and a Mac window
/// dragged narrow should behave the same way. `PlatformEnvironment` decides only
/// what the platform is *capable* of.
///
/// This view also hosts the app-wide diagnostics hooks — memory pressure, clean
/// shutdown, and the "crashed last launch?" prompt — because they belong to the
/// scene rather than to any one screen.
struct RootShell: View {
    let environment: AppEnvironment

    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.scenePhase) private var scenePhase
    @State private var router = Router()
    @State private var crashReportOffered = false
    @State private var crashReport: DiagnosticsReport?

    /// Regular width earns the split view; compact gets the tab bar.
    ///
    /// `horizontalSizeClass` is `nil` on macOS, where there is no size-class
    /// notion at all — and a Mac always wants the split view, so the default is
    /// the right answer rather than a fallback.
    private var usesSplitShell: Bool {
        horizontalSizeClass != .compact
    }

    var body: some View {
        Group {
            if usesSplitShell {
                SplitShell(environment: environment, router: router)
            } else {
                CompactShell(environment: environment, router: router)
            }
        }
        .environment(router)
        // One tint for the whole scene. Set here rather than per screen because
        // an accent is a property of the app, and thirty screens each choosing
        // their own is how an app ends up with three blues.
        .tint(CapsuleTheme.Colors.accent)
        // Menu commands live outside every view hierarchy, so they read the
        // router from the *focused* scene rather than the environment. This is
        // what makes a command act on the window you are looking at.
        .focusedSceneValue(\.router, router)
        .onChange(of: usesSplitShell, initial: true) { _, isSplit in
            // Keep the router's own idea of the shell in step, so a command that
            // targets the detail column knows whether one exists.
            router.shell = isSplit ? .split : .stacked
        }
        .onOpenURL { url in
            // Deliberately ignores the return value: an unrecognised link is not
            // an error the user needs to see, and `open(_:)` never stores the
            // fragment, so nothing leaks by dropping it.
            _ = router.open(url)
        }
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
        .alert("app.diagnostics.crash_alert.title", isPresented: $crashReportOffered) {
            Button("app.diagnostics.crash_alert.send") {
                Task {
                    let bundle = await environment.diagnostics.makeReportBundle()
                    await environment.diagnostics.acknowledgeCrashReport()
                    if let data = try? bundle.jsonData() {
                        crashReport = DiagnosticsReport(data: data)
                    }
                }
            }
            Button("app.common.not_now", role: .cancel) {
                Task { await environment.diagnostics.acknowledgeCrashReport() }
            }
        } message: {
            Text("app.diagnostics.crash_alert.body")
        }
        .sheet(item: $crashReport) { DiagnosticsReportView(report: $0) }
    }
}
