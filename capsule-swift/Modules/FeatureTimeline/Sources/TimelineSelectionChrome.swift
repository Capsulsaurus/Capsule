import SwiftUI

// The Library's *selection* chrome — the confirmations a multi-select action
// puts in front of the user.
//
// Separated from `TimelineRootView` for the reason `TimelineImportChrome` was:
// none of it is about drawing a grid, and that view's body had grown past the
// length the lint allows. A modifier with explicit inputs rather than an
// extension, because Swift's `private` is file-scoped and an extension in
// another file cannot reach the view's `@State`.

// MARK: - Delete confirmation

/// The "Delete N Items?" sheet the selection bar's delete action raises.
///
/// Both strings are whole-message catalog plurals resolved through
/// `String(localized:defaultValue:)`: the count agrees with the noun in the
/// arm rather than being interpolated into a fixed English one, and the
/// interpolation in `defaultValue` is what supplies the plural argument.
private struct DeleteSelectionConfirmation: ViewModifier {
    let count: Int
    @Binding var isPresented: Bool
    let onConfirm: () -> Void

    func body(content: Content) -> some View {
        content.confirmationDialog(
            String(
                localized: "app.timeline.delete_selected.title",
                defaultValue: "Delete \(count) Items?"
            ),
            isPresented: $isPresented,
            titleVisibility: .visible
        ) {
            Button(
                String(
                    localized: "app.timeline.delete_selected.confirm",
                    defaultValue: "Delete \(count) Items"
                ),
                role: .destructive,
                action: onConfirm
            )
        }
    }
}

extension View {
    /// Confirm the destructive half of a multi-select action.
    func deleteSelectionConfirmation(
        count: Int,
        isPresented: Binding<Bool>,
        onConfirm: @escaping () -> Void
    ) -> some View {
        modifier(DeleteSelectionConfirmation(
            count: count,
            isPresented: isPresented,
            onConfirm: onConfirm
        ))
    }
}
