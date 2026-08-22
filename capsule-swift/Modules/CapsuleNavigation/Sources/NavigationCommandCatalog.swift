import Foundation

// MARK: - The command table

/// Every menu command the app offers, in menu order.
///
/// Two commands here already exist in the app as `NotificationCenter` posts
/// (`ios.menu.import`, `ios.menu.culling_review`) and keep their catalog keys,
/// so replacing that stopgap with ``Router/perform(_:)`` is not a re-translation.
///
/// The ⌘1…⌘4 zoom levels follow the ordering every photo app on the platform
/// uses — widest zoom on ⌘1, the flat grid on ⌘4 — because the shortcut is
/// muscle memory borrowed from elsewhere, not something to be clever about.
public extension NavigationCommand {
    /// The whole menu surface, in order.
    static let all: [NavigationCommand] = [
        NavigationCommand(
            action: .importMedia,
            titleKey: "ios.menu.import", systemImage: "square.and.arrow.down",
            shortcut: CommandShortcut("i"), placement: .file
        ),
        NavigationCommand(
            action: .exportSelection,
            titleKey: "ios.menu.export", systemImage: "square.and.arrow.up",
            shortcut: CommandShortcut("e", modifiers: [.command, .shift]), placement: .file
        ),
        NavigationCommand(
            action: .selectAll,
            titleKey: "ios.menu.select_all", systemImage: "checkmark.circle",
            shortcut: CommandShortcut("a"), placement: .edit
        ),
        NavigationCommand(
            action: .toggleSidebar,
            titleKey: "ios.menu.toggle_sidebar", systemImage: "sidebar.leading",
            shortcut: CommandShortcut("s", modifiers: [.command, .control]), placement: .view
        ),
        NavigationCommand(
            action: .cullingReview,
            titleKey: "ios.menu.culling_review", systemImage: "checkmark.circle",
            shortcut: CommandShortcut("r", modifiers: [.command, .shift]), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.years),
            titleKey: "ios.menu.zoom.years", systemImage: "calendar",
            shortcut: CommandShortcut("1"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.months),
            titleKey: "ios.menu.zoom.months", systemImage: "calendar",
            shortcut: CommandShortcut("2"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.days),
            titleKey: "ios.menu.zoom.days", systemImage: "calendar.day.timeline.left",
            shortcut: CommandShortcut("3"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.all),
            titleKey: "ios.menu.zoom.all", systemImage: "square.grid.3x3",
            shortcut: CommandShortcut("4"), placement: .view
        ),
        NavigationCommand(
            action: .previousAsset,
            titleKey: "ios.menu.previous_asset", systemImage: "chevron.left",
            shortcut: CommandShortcut(.leftArrow, modifiers: []), placement: .view
        ),
        NavigationCommand(
            action: .nextAsset,
            titleKey: "ios.menu.next_asset", systemImage: "chevron.right",
            shortcut: CommandShortcut(.rightArrow, modifiers: []), placement: .view
        ),
        NavigationCommand(
            action: .closeWindow,
            titleKey: "ios.menu.close_window", systemImage: "xmark",
            shortcut: CommandShortcut("w"), placement: .window
        ),
    ]

    /// The commands in one menu, in order.
    static func commands(in placement: CommandPlacement) -> [NavigationCommand] {
        all.filter { $0.placement == placement }
    }

    /// The command for an action, if the action has one.
    static func command(for action: NavigationAction) -> NavigationCommand? {
        all.first { $0.action == action }
    }
}
