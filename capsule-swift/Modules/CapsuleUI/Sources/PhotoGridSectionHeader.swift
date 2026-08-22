import SwiftUI

/// A pinned section header — a date label over a chrome material so it stays
/// legible while photos scroll beneath it.
///
/// `.bar` rather than Liquid Glass on purpose: the header is pinned *over photo
/// content*, and the HIG reserves glass for the control layer. The material is
/// the honest choice here and behaves identically on both platforms.
struct PhotoGridSectionHeader: View {
    let title: String

    var body: some View {
        Text(title)
            .font(.system(size: 15, weight: .semibold))
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            .padding(.horizontal, CapsuleTheme.Spacing.medium)
            .padding(.vertical, CapsuleTheme.Spacing.small)
            .background(.bar)
    }
}
