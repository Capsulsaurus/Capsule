import AssetKit
import CapsuleFoundation
import CapsuleUI
import Observation
import SwiftUI

// MARK: - AssetCaptionEditor

/// The caption field's state: what is typed, what the store holds, and whether
/// the last save was refused.
///
/// Its own type so the save-and-refusal path is tested without a view. A
/// refused write is never reported saved: the typed text stays in the field,
/// ``saveFailed`` turns on, and the next commit tries again.
@MainActor
@Observable
final class AssetCaptionEditor {
    /// What is in the field.
    var text = ""
    /// Whether the last write was refused. Cleared by the next commit attempt.
    private(set) var saveFailed = false
    /// What the store last accepted or reported, so a blur with no edit writes
    /// nothing.
    private(set) var committed = ""

    private let store: any CaptionStore
    private var assetID: AssetID?

    init(store: any CaptionStore) {
        self.store = store
    }

    /// Show `id`'s stored caption, forgetting any refusal from another asset.
    func load(_ id: AssetID) async {
        assetID = id
        saveFailed = false
        let stored = await store.caption(for: id) ?? ""
        guard assetID == id else { return }
        text = stored
        committed = stored
    }

    /// Write the caption, if it actually changed. Clearing is `nil`, never an
    /// empty string.
    func commit() async {
        guard let id = assetID else { return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != committed else { return }
        let previous = committed
        committed = trimmed
        saveFailed = false
        do {
            try await store.setCaption(trimmed.isEmpty ? nil : trimmed, for: id)
        } catch {
            CapsuleLog.interface.error("caption save refused: \(String(describing: error), privacy: .public)")
            // Another asset may be on screen by now; its field is not this one's
            // to mark.
            guard assetID == id else { return }
            committed = previous
            saveFailed = true
        }
    }
}

// MARK: - AssetCaptionField

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

    @State private var editor: AssetCaptionEditor
    @FocusState private var isFocused: Bool

    init(assetID: AssetID, store: any CaptionStore) {
        self.assetID = assetID
        _editor = State(initialValue: AssetCaptionEditor(store: store))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            TextField(
                "app.viewer.info.caption_placeholder",
                text: $editor.text,
                axis: .vertical
            )
            .font(CapsuleTheme.Typography.body)
            .lineLimit(1 ... 4)
            .focused($isFocused)
            .submitLabel(.done)
            .accessibilityLabel(Text("app.viewer.info.caption.accessibility"))
            if editor.saveFailed {
                Label("app.viewer.info.caption.save_failed", systemImage: "exclamationmark.triangle")
                    .font(CapsuleTheme.Typography.detail)
                    .foregroundStyle(.orange)
                    .accessibilityElement(children: .combine)
            }
        }
        .task(id: assetID) { await editor.load(assetID) }
        .onChange(of: isFocused) { _, focused in
            if !focused { Task { await editor.commit() } }
        }
        .onSubmit { Task { await editor.commit() } }
    }
}
