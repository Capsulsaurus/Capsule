import SwiftUI

/// A standard placeholder shown by feature screens that are not yet implemented.
///
/// Lets the app shell be assembled and run before individual features exist.
public struct PlaceholderView: View {
    private let title: String
    private let systemImage: String
    private let message: String

    public init(title: String, systemImage: String, message: String) {
        self.title = title
        self.systemImage = systemImage
        self.message = message
    }

    public var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: systemImage)
        } description: {
            Text(message)
        }
    }
}
