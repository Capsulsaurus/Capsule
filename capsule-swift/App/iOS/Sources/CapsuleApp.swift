import CapsuleFoundation
import SwiftUI

/// The Capsule iOS / iPadOS application entry point.
@main
struct CapsuleApp: App {
    init() {
        CapsuleLog.app.info("Capsule launching")
    }

    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}
