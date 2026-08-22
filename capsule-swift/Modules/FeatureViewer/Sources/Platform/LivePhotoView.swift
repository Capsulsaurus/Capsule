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

        func makeUIView(context _: Context) -> PHLivePhotoView {
            let view = PHLivePhotoView()
            view.contentMode = .scaleAspectFit
            return view
        }

        func updateUIView(_ view: PHLivePhotoView, context _: Context) {
            guard view.livePhoto !== livePhoto else { return }
            view.livePhoto = livePhoto
            view.startPlayback(with: .hint)
        }
    }

#else

    /// A `PHLivePhotoView` bridged into SwiftUI; plays the motion hint on appear.
    struct LivePhotoView: NSViewRepresentable {
        let livePhoto: PHLivePhoto

        /// macOS `PHLivePhotoView` has no `contentMode`; it always fits its
        /// bounds preserving aspect, which is the behaviour iOS opts into.
        func makeNSView(context _: Context) -> PHLivePhotoView {
            PHLivePhotoView()
        }

        func updateNSView(_ view: PHLivePhotoView, context _: Context) {
            guard view.livePhoto !== livePhoto else { return }
            view.livePhoto = livePhoto
            view.startPlayback(with: .hint)
        }
    }

#endif
