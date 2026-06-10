import SwiftUI

/// A labelled "coming soon" destination for Collections sections that later
/// phases fill in (Places, People & Pets, Memories, Duplicates, …). Keeping the
/// navigation rows live — but honest about being unfinished — preserves the
/// Apple Photos information architecture without faking functionality.
public struct CollectionPlaceholderView: View {
    private let title: String
    private let systemImage: String
    private let message: String

    public init(title: String, systemImage: String, message: String) {
        self.title = title
        self.systemImage = systemImage
        self.message = message
    }

    public var body: some View {
        ContentUnavailableView(
            title,
            systemImage: systemImage,
            description: Text(message)
        )
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
