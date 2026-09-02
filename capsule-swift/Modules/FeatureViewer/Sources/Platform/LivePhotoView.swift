import Photos
import PhotosUI
import SwiftUI

// `PHLivePhotoView` ships on both platforms but is a `UIView` on iOS and an
// `NSView` on macOS, so the representable protocol — and therefore the whole
// type — has to be written twice. The two halves are kept side by side here so
// the shared behaviour (set the photo, hint once) is obviously identical.
//
// Note the file names `PHLivePhotoView` but never `UIView`/`NSView`: the
// associated type is inferred from the factory's return type, which is why no
// `import UIKit` / `import AppKit` is needed.

#if os(iOS)

    /// A `PHLivePhotoView` bridged into SwiftUI; plays the motion hint on appear.
    struct LivePhotoView: UIViewRepresentable {
        let livePhoto: PHLivePhoto
        /// Bumped by the LIVE badge to replay. A counter rather than a `Bool`
        /// because playback is an event, not a state: the second replay has to
        /// be distinguishable from the first.
        var playbackTicket: Int = 0

        func makeUIView(context _: Context) -> PHLivePhotoView {
            let view = PHLivePhotoView()
            view.contentMode = .scaleAspectFit
            return view
        }

        func updateUIView(_ view: PHLivePhotoView, context: Context) {
            if view.livePhoto !== livePhoto {
                view.livePhoto = livePhoto
                view.startPlayback(with: .hint)
                context.coordinator.lastTicket = playbackTicket
                return
            }
            guard playbackTicket != context.coordinator.lastTicket else { return }
            context.coordinator.lastTicket = playbackTicket
            view.startPlayback(with: .full)
        }

        func makeCoordinator() -> Coordinator { Coordinator() }

        final class Coordinator {
            var lastTicket = 0
        }
    }

#else

    /// A `PHLivePhotoView` bridged into SwiftUI; plays the motion hint on appear.
    struct LivePhotoView: NSViewRepresentable {
        let livePhoto: PHLivePhoto
        /// Bumped by the LIVE badge to replay. See the iOS twin.
        var playbackTicket: Int = 0

        /// macOS `PHLivePhotoView` has no `contentMode`; it always fits its
        /// bounds preserving aspect, which is the behaviour iOS opts into.
        func makeNSView(context _: Context) -> PHLivePhotoView {
            PHLivePhotoView()
        }

        func updateNSView(_ view: PHLivePhotoView, context: Context) {
            if view.livePhoto !== livePhoto {
                view.livePhoto = livePhoto
                view.startPlayback(with: .hint)
                context.coordinator.lastTicket = playbackTicket
                return
            }
            guard playbackTicket != context.coordinator.lastTicket else { return }
            context.coordinator.lastTicket = playbackTicket
            view.startPlayback(with: .full)
        }

        func makeCoordinator() -> Coordinator { Coordinator() }

        final class Coordinator {
            var lastTicket = 0
        }
    }

#endif
