#if os(macOS)

    import AppKit
    import SwiftUI

    // The AppKit views that host this island's SwiftUI content.
    //
    // Split out of `PlatformCollectionController+macOS.swift` because they are a
    // separate concern from the controller — a cell and a supplementary view
    // that turn SwiftUI into AppKit, with no knowledge of sections, snapshots,
    // or scrolling — and because that file had grown past the length the lint
    // allows.

    // MARK: - Hosting item

    /// An `NSCollectionViewItem` whose entire body is a hosted SwiftUI view.
    ///
    /// Re-hosting on dequeue is what makes the SwiftUI content's own `task`
    /// cancellation work: the recycled item is handed the next item's content
    /// before it is shown, so the previous subtree is torn down and its
    /// in-flight thumbnail decode cancelled — exactly what `prepareForReuse`
    /// did by hand in the UIKit-only implementation this replaced.
    final class PlatformHostingItem: NSCollectionViewItem {
        static let identifier = NSUserInterfaceItemIdentifier("PlatformHostingItem")

        private let hostingView = NSHostingView(rootView: AnyView(EmptyView()))

        override func loadView() {
            let container = NSView()
            hostingView.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(hostingView)
            NSLayoutConstraint.activate([
                hostingView.topAnchor.constraint(equalTo: container.topAnchor),
                hostingView.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                hostingView.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                hostingView.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
            view = container
        }

        func host(_ content: AnyView) {
            hostingView.rootView = content
        }
    }

    /// The supplementary-view twin of ``PlatformHostingItem``, used for pinned
    /// section headers.
    /// Conforms to `NSCollectionViewElement` because AppKit's
    /// `supplementaryViewProvider` is typed `(NSView & NSCollectionViewElement)?`,
    /// not plain `NSView` as the item provider is. The protocol has no required
    /// members — the conformance is what makes the view usable as a supplementary
    /// element at all.
    final class PlatformHostingSupplementaryView: NSView, NSCollectionViewElement {
        static let identifier = NSUserInterfaceItemIdentifier("PlatformHostingSupplementaryView")

        private let hostingView = NSHostingView(rootView: AnyView(EmptyView()))

        override init(frame frameRect: NSRect) {
            super.init(frame: frameRect)
            hostingView.translatesAutoresizingMaskIntoConstraints = false
            addSubview(hostingView)
            NSLayoutConstraint.activate([
                hostingView.topAnchor.constraint(equalTo: topAnchor),
                hostingView.bottomAnchor.constraint(equalTo: bottomAnchor),
                hostingView.leadingAnchor.constraint(equalTo: leadingAnchor),
                hostingView.trailingAnchor.constraint(equalTo: trailingAnchor),
            ])
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("PlatformHostingSupplementaryView is not loaded from a nib")
        }

        func host(_ content: AnyView) {
            hostingView.rootView = content
        }
    }

#endif
