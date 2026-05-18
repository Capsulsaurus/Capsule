import OSLog

/// Centralised `OSLog` loggers — one per subsystem area.
///
/// Capsule logs critical processes verbosely and in structured form so they are
/// queryable after the fact (Console.app, or `log show --predicate
/// 'subsystem == "com.justin13888.capsule"'`). Use `.info` for lifecycle events
/// and `.debug`/`.trace` aggressively on hot and critical paths.
public enum CapsuleLog {
    /// The shared logging subsystem; also used by the Rust core via `oslog`.
    public static let subsystem = "com.justin13888.capsule"

    public static let app = Logger(subsystem: subsystem, category: "app")
    public static let catalog = Logger(subsystem: subsystem, category: "catalog")
    public static let managedStore = Logger(subsystem: subsystem, category: "managed-store")
    public static let assetKit = Logger(subsystem: subsystem, category: "asset-kit")
    public static let imagePipeline = Logger(subsystem: subsystem, category: "image-pipeline")
    public static let interface = Logger(subsystem: subsystem, category: "ui")
}
