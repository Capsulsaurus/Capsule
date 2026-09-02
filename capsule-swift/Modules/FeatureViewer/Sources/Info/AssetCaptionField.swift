import AssetKit
import CapsuleFoundation
import CapsuleUI
import SwiftUI

/// The caption, editable in place at the top of the info panel.
///
/// The first thing in the panel because it is the only thing in it the reader
/// *wrote*. Everything below is what the camera recorded.
///
/// Commits on blur and on submit rather than on every keystroke: a caption is a
/// CRDT register with a superseded log behind it, and one write per character
/// would fill that log with a hundred versions of the same sentence.
struct AssetCaptionField: View {
    let assetID: AssetID
    let store: any CaptionStore

    @State private var text = ""
    /// What the store last told us, so a blur with no edit writes nothing.
    @State private var committed = ""
    @FocusState private var isFocused: Bool

    var body: some View {
        TextField(
            "app.viewer.info.caption_placeholder",
            text: $text,
            axis: .vertical
        )
        .font(CapsuleTheme.Typography.body)
        .lineLimit(1 ... 4)
        .focused($isFocused)
        .submitLabel(.done)
        .accessibilityLabel(Text("app.viewer.info.caption.accessibility"))
        .task(id: assetID) { await load() }
        .onChange(of: isFocused) { _, focused in
            if !focused { commit() }
        }
        .onSubmit { commit() }
    }

    private func load() async {
        let stored = await store.caption(for: assetID) ?? ""
        text = stored
        committed = stored
    }

    /// Write the caption, if it actually changed.
    private func commit() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != committed else { return }
        committed = trimmed
        let store = store
        let assetID = assetID
        Task { try? await store.setCaption(trimmed.isEmpty ? nil : trimmed, for: assetID) }
    }
}
