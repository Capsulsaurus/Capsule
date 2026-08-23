import SwiftUI

/// A destination whose route names an identifier the screen cannot take.
///
/// ``Route`` payloads are identifiers, never models, so a screen such as
/// `QuarantineDetailView` — which takes the item itself — needs its id resolved
/// through a port before it can be constructed. That resolution is asynchronous
/// and can legitimately come back empty: the drop was adopted from another
/// device, the quarantined item was discarded, the album was deleted while the
/// route sat in a restored navigation stack.
///
/// Somewhere has to hold the in-flight state for that, and ``RouteDestination``
/// deliberately holds none. This does, once, rather than each route growing its
/// own bespoke wrapper.
struct ResolvedDestination<Value: Sendable, Content: View>: View {
    /// The catalog key for the destination's own name, shown while resolving and
    /// when resolution finds nothing.
    let titleKey: String
    /// The SF Symbol the owning section carries elsewhere, so the empty state
    /// still reads as that place.
    let systemImage: String
    /// Reads the identifier back through a port. `nil` means "no longer there",
    /// which is a normal outcome rather than an error.
    let resolve: @Sendable () async -> Value?
    /// `@MainActor` because what it builds is handed the composition root's
    /// ports, and those are main-actor state.
    @ViewBuilder let content: @MainActor (Value) -> Content

    @State private var resolved: Value?
    @State private var hasResolved = false

    var body: some View {
        Group {
            if let resolved {
                content(resolved)
            } else if hasResolved {
                missing
            } else {
                ProgressView()
                    .controlSize(.large)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task {
            resolved = await resolve()
            hasResolved = true
        }
    }

    private var missing: some View {
        ContentUnavailableView {
            Label(LocalizedStringKey(titleKey), systemImage: systemImage)
        } description: {
            Text("ios.destination.missing.body")
        }
        .navigationTitle(LocalizedStringKey(titleKey))
    }
}
