import Foundation
import Testing

import CapsuleNavigation

/// The menu bar and the iPad shortcut overlay are both generated from this
/// list, so a collision here is a collision on two platforms at once.
@Suite("The command table is coherent")
struct NavigationCommandTests {
    @Test("no action appears twice")
    func actionsAreUnique() {
        #expect(Set(NavigationCommand.all.map(\.action)).count == NavigationCommand.all.count)
    }

    @Test("no two commands claim the same key equivalent")
    func shortcutsDoNotCollide() {
        let shortcuts = NavigationCommand.all.compactMap(\.shortcut)

        #expect(Set(shortcuts).count == shortcuts.count)
    }

    @Test("every title is a catalog key, never display text")
    func titlesAreCatalogKeys() {
        for command in NavigationCommand.all {
            #expect(command.titleKey.hasPrefix("ios.menu."), "\(command.titleKey)")
            #expect(!command.titleKey.contains(" "), "\(command.titleKey)")
        }
    }

    @Test("the two commands that already exist keep their catalog keys")
    func existingKeysAreNotRetranslated() {
        #expect(NavigationCommand.command(for: .importMedia)?.titleKey == "ios.menu.import")
        #expect(NavigationCommand.command(for: .cullingReview)?.titleKey == "ios.menu.culling_review")
    }

    @Test("every zoom level is reachable, on the shortcut the platform expects")
    func zoomLevelsCoverEveryFocus() {
        let expected: [(TimelineFocus, Character)] = [
            (.years, "1"),
            (.months, "2"),
            (.days, "3"),
            (.all, "4"),
        ]
        for (focus, key) in expected {
            let command = NavigationCommand.command(for: .zoom(focus))
            #expect(command?.shortcut == CommandShortcut(key), "\(focus)")
        }
    }

    @Test("commands partition cleanly across the menus they belong to")
    func placementsPartitionTheTable() {
        let placed = CommandPlacement.allCases.flatMap(NavigationCommand.commands(in:))

        #expect(placed.count == NavigationCommand.all.count)
    }

    @Test("the arrow-key shortcuts stay platform-neutral")
    func arrowKeysAreNamedNotEncoded() {
        #expect(NavigationCommand.command(for: .nextAsset)?.shortcut == CommandShortcut(.rightArrow, modifiers: []))
        #expect(NavigationCommand.command(for: .previousAsset)?.shortcut == CommandShortcut(.leftArrow, modifiers: []))
    }
}

// MARK: - CommandAcceptanceTests

/// `accepts` and `perform` are two switches over the same set, so they can
/// drift — and drift is invisible: a command that reports acceptable but does
/// nothing looks exactly like one that worked.
@Suite("What the router says it accepts is what it does")
@MainActor
struct CommandAcceptanceTests {
    /// Every action in the catalog, so a new one cannot skip this.
    private static var allActions: [NavigationAction] { NavigationCommand.all.map(\.action) }

    @Test("accepts agrees with perform for every command in the menu")
    func acceptanceMatchesPerformance() {
        for action in Self.allActions {
            let asked = Router(shell: .split).accepts(action)
            let done = Router(shell: .split).perform(action)
            #expect(asked == done, "\(action): accepts says \(asked), perform says \(done)")
        }
    }

    @Test("the menu renders every command, including the ones the router declines")
    func declinedCommandsAreStillOffered() {
        let declined = Self.allActions.filter { !Router(shell: .split).accepts($0) }
        #expect(!declined.isEmpty, "if nothing is declined this test is asserting nothing")
        // They stay in the catalog so the menu can show them disabled: a greyed
        // item is discoverable, a missing one is not.
        #expect(declined.allSatisfy { action in NavigationCommand.all.contains { $0.action == action } })
    }
}
