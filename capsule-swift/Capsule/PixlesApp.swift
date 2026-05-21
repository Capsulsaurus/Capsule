import Shared
import SwiftData
import SwiftUI

@main
struct CapsuleApp: App {
    init() {
        do {
            try KoinKt.doInitKoin()
        } catch {
            print("Error initializing Koin: \(error)") // TODO:
            // You might want to handle this more gracefully depending on your app's requirements
            // For example, showing an error UI or attempting recovery
        }
    }

    var sharedModelContainer: ModelContainer = {
        let schema = Schema([
            Item.self,
        ])
        let modelConfiguration = ModelConfiguration(schema: schema, isStoredInMemoryOnly: false)

        do {
            return try ModelContainer(for: schema, configurations: [modelConfiguration])
        } catch {
            fatalError("Could not create ModelContainer: \(error)")
        }
    }()

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .modelContainer(sharedModelContainer)
    }
}
