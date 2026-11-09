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
            titleKey: "apple.menu.import", systemImage: "square.and.arrow.down",
            shortcut: CommandShortcut("i"), placement: .file
        ),
        NavigationCommand(
            action: .exportSelection,
            titleKey: "apple.menu.export", systemImage: "square.and.arrow.up",
            shortcut: CommandShortcut("e", modifiers: [.command, .shift]), placement: .file
        ),
        NavigationCommand(
            action: .selectAll,
            titleKey: "apple.menu.select_all", systemImage: "checkmark.circle",
            shortcut: CommandShortcut("a"), placement: .edit
        ),
        NavigationCommand(
            action: .toggleSidebar,
            titleKey: "apple.menu.toggle_sidebar", systemImage: "sidebar.leading",
            shortcut: CommandShortcut("s", modifiers: [.command, .control]), placement: .view
        ),
        NavigationCommand(
            action: .cullingReview,
            titleKey: "apple.menu.culling_review", systemImage: "checkmark.circle",
            shortcut: CommandShortcut("r", modifiers: [.command, .shift]), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.years),
            titleKey: "apple.menu.zoom.years", systemImage: "calendar",
            shortcut: CommandShortcut("1"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.months),
            titleKey: "apple.menu.zoom.months", systemImage: "calendar",
            shortcut: CommandShortcut("2"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.days),
            titleKey: "apple.menu.zoom.days", systemImage: "calendar.day.timeline.left",
            shortcut: CommandShortcut("3"), placement: .view
        ),
        NavigationCommand(
            action: .zoom(.all),
            titleKey: "apple.menu.zoom.all", systemImage: "square.grid.3x3",
            shortcut: CommandShortcut("4"), placement: .view
        ),
        NavigationCommand(
            action: .previousAsset,
            titleKey: "apple.menu.previous_asset", systemImage: "chevron.left",
            shortcut: CommandShortcut(.leftArrow, modifiers: []), placement: .view
        ),
        NavigationCommand(
            action: .nextAsset,
            titleKey: "apple.menu.next_asset", systemImage: "chevron.right",
            shortcut: CommandShortcut(.rightArrow, modifiers: []), placement: .view
        ),
        NavigationCommand(
            action: .closeWindow,
            titleKey: "apple.menu.close_window", systemImage: "xmark",
            shortcut: CommandShortcut("w"), placement: .window
        ),
    ] + Self.culling

    /// The cull pass: one bare key per verdict and per stack move, so a whole
    /// pass runs with one hand on the keyboard and the arrows
    /// (``NavigationAction/nextAsset``, ``NavigationAction/previousAsset``) in
    /// the other.
    ///
    /// P, U and X are the pick, unflag and reject keys every culling tool on the
    /// platform already taught photographers. Expand and collapse are E and C.
    /// The sweep alone carries ⌘, on ⌘⌫ — the platform's move-to-trash key —
    /// because it is the one destructive step, and a bare key would be one stray
    /// press from a confirmation prompt.
    private static let culling: [NavigationCommand] = [
        NavigationCommand(
            action: .cullPick,
            titleKey: "apple.menu.culling.pick", systemImage: "flag",
            shortcut: CommandShortcut("p", modifiers: []), placement: .culling
        ),
        NavigationCommand(
            action: .cullNeutral,
            titleKey: "apple.menu.culling.unflag", systemImage: "flag.slash",
            shortcut: CommandShortcut("u", modifiers: []), placement: .culling
        ),
        NavigationCommand(
            action: .cullReject,
            titleKey: "apple.menu.culling.reject", systemImage: "xmark.circle",
            shortcut: CommandShortcut("x", modifiers: []), placement: .culling
        ),
        NavigationCommand(
            action: .expandStack,
            titleKey: "apple.menu.culling.expand_stack", systemImage: "square.stack.3d.down.right",
            shortcut: CommandShortcut("e", modifiers: []), placement: .culling
        ),
        NavigationCommand(
            action: .collapseStack,
            titleKey: "apple.menu.culling.collapse_stack", systemImage: "square.stack.3d.up",
            shortcut: CommandShortcut("c", modifiers: []), placement: .culling
        ),
        NavigationCommand(
            action: .rejectSweep,
            titleKey: "apple.menu.culling.sweep", systemImage: "trash",
            shortcut: CommandShortcut(.delete), placement: .culling
        ),
    ]

    /// Every action the culling surface answers, paging included: the list its
    /// in-app shortcut sheet shows, in the order a pass uses them.
    static let cullingPass: [NavigationCommand] =
        [NavigationAction.nextAsset, .previousAsset].compactMap { Self.command(for: $0) } + Self.culling

    /// The commands in one menu, in order.
    static func commands(in placement: CommandPlacement) -> [NavigationCommand] {
        all.filter { $0.placement == placement }
    }

    /// The command for an action, if the action has one.
    static func command(for action: NavigationAction) -> NavigationCommand? {
        all.first { $0.action == action }
    }
}
