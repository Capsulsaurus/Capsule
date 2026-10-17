import Foundation

// MARK: - NavigationAction

/// What a menu item or keyboard shortcut asks for, named platform-neutrally.
///
/// The Mac menu bar and the iPad hardware-keyboard overlay want the same
/// actions with the same shortcuts. Declaring them once as data means the two
/// surfaces are *generated* from one list rather than written twice and left to
/// diverge — which is how an iPad ends up missing ⌘A, or a Mac ends up with a
/// menu item whose shortcut moved.
///
/// The router consumes the navigational members (``Router/perform(_:)``) and
/// declines the rest, which are the focused scene's business: exporting needs a
/// selection, paging needs the viewer's position in its sequence, and closing a
/// window is not navigation at all.
public enum NavigationAction: Sendable, Hashable {
    /// Bring photos in.
    case importMedia
    /// Write the current selection out.
    case exportSelection
    /// Select everything in the focused collection.
    case selectAll
    /// Show or hide the sidebar.
    case toggleSidebar
    /// Enter the keyboard-driven cull pass over whatever is showing.
    case cullingReview
    /// Jump the timeline to a zoom level — ⌘1 through ⌘4.
    ///
    /// Modelled as a ``TimelineFocus`` rather than an opaque "level 1…4"
    /// because that is what the four levels *are*, and it lets the router act on
    /// the command by navigating rather than by broadcasting an integer that
    /// some view has to interpret.
    case zoom(TimelineFocus)
    /// Page forward in the viewer's sequence.
    case nextAsset
    /// Page backward in the viewer's sequence.
    case previousAsset
    /// Close the focused window.
    case closeWindow
}

// MARK: - Shortcuts

/// The modifier keys a shortcut needs, without importing a UI framework.
public struct CommandModifiers: OptionSet, Sendable, Hashable {
    public let rawValue: Int

    public init(rawValue: Int) {
        self.rawValue = rawValue
    }

    public static let command = CommandModifiers(rawValue: 1 << 0)
    public static let shift = CommandModifiers(rawValue: 1 << 1)
    public static let option = CommandModifiers(rawValue: 1 << 2)
    public static let control = CommandModifiers(rawValue: 1 << 3)
}

/// The key half of a shortcut.
///
/// An enum rather than a bare `Character` because the arrow keys have no
/// printable character: SwiftUI spells them `KeyEquivalent.leftArrow`, and
/// encoding them as private-use scalars here would make this module carry a
/// UIKit implementation detail it is not allowed to import.
public enum CommandKey: Sendable, Hashable {
    case character(Character)
    case leftArrow
    case rightArrow
}

/// A key equivalent, as data. The app maps this to SwiftUI's
/// `KeyboardShortcut`; nothing here knows that type exists.
public struct CommandShortcut: Sendable, Hashable {
    public let key: CommandKey
    public let modifiers: CommandModifiers

    public init(_ key: CommandKey, modifiers: CommandModifiers = .command) {
        self.key = key
        self.modifiers = modifiers
    }

    /// The common case: a letter or digit with ⌘ and optional extras.
    public init(_ character: Character, modifiers: CommandModifiers = .command) {
        self.init(.character(character), modifiers: modifiers)
    }
}

// MARK: - Placement

/// Which menu a command belongs to.
///
/// Only the four menus Capsule actually populates. The Mac menu bar maps these
/// to `CommandGroup` positions; the iPad overlay uses them purely as grouping,
/// which is why this is a neutral name rather than a `CommandGroupPlacement`.
public enum CommandPlacement: String, Sendable, Hashable, CaseIterable {
    case file
    case edit
    case view
    case window
}

// MARK: - NavigationCommand

/// One menu item: what it does, what it is called, and how to type it.
///
/// A value type on purpose. The SwiftUI `Commands` builder that renders these
/// lives in the app target, so this module stays free of view code and the
/// command list stays unit-testable — shortcut collisions are a test, not a
/// thing you discover by pulling down a menu.
public struct NavigationCommand: Sendable, Hashable, Identifiable {
    /// What firing the item asks for.
    public let action: NavigationAction
    /// Catalog key for the item's title. Never literal text.
    public let titleKey: String
    /// An SF Symbol name — not translatable.
    public let systemImage: String
    /// The key equivalent, or `nil` for a menu item with no shortcut.
    public let shortcut: CommandShortcut?
    /// Which menu it appears under.
    public let placement: CommandPlacement

    public var id: NavigationAction { action }

    public init(
        action: NavigationAction,
        titleKey: String,
        systemImage: String,
        shortcut: CommandShortcut?,
        placement: CommandPlacement
    ) {
        self.action = action
        self.titleKey = titleKey
        self.systemImage = systemImage
        self.shortcut = shortcut
        self.placement = placement
    }
}
