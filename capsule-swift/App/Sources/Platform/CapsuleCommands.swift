import CapsuleNavigation
import SwiftUI

// MARK: - RouterFocusedValue

/// The router belonging to the focused scene.
///
/// Menu commands live outside every view hierarchy, so they cannot read the
/// environment. `@FocusedValue` is how SwiftUI hands them the *focused window's*
/// state — which is the right granularity: once detached viewer windows land, a
/// command has to act on the window you are looking at rather than on whichever
/// router a singleton happened to hold.
struct RouterFocusedValue: FocusedValueKey {
    typealias Value = Router
}

extension FocusedValues {
    var router: Router? {
        get { self[RouterFocusedValue.self] }
        set { self[RouterFocusedValue.self] = newValue }
    }
}

// MARK: - CapsuleCommands

/// The app's menu commands, rendered from the command catalog.
///
/// A Mac app is judged partly on whether its menus are real — whether the
/// things you can do are discoverable there and carry the shortcuts you would
/// guess. These are that surface, and they are *generated* from
/// ``NavigationCommand/all`` rather than written out here, so the menu bar and
/// the iPad hardware-keyboard overlay cannot drift from each other or from the
/// tests that assert no two commands claim the same key.
///
/// Before this, the menu held two hand-written items that posted
/// `NotificationCenter` notifications **nothing observed** — ⌘I and ⇧⌘R were
/// silent no-ops — while the catalog declaring all twelve, and the
/// `Router.perform` that consumes them, both already existed. Only the renderer
/// was missing.
///
/// Commands the router declines (export, select-all, paging, close) belong to
/// the focused scene rather than to navigation. They are rendered so the menu
/// tells the truth about what the app does, and disabled until the screen that
/// owns them can answer — a greyed item is discoverable, a missing one is not.
struct CapsuleCommands: Commands {
    /// The documentation site the Help menu points at.
    private static let documentationURL = "https://capsule.photos/docs"

    @FocusedValue(\.router) private var router: Router?

    var body: some Commands {
        // Replace the stock "New Window" group: Capsule's File menu is about
        // getting photos in and out, not documents.
        CommandGroup(replacing: .newItem) {
            items(in: .file)
        }

        CommandGroup(replacing: .help) {
            if let docs = URL(string: Self.documentationURL) {
                Link("apple.menu.help", destination: docs)
            }
        }

        CommandGroup(after: .pasteboard) {
            items(in: .edit)
        }

        CommandGroup(after: .toolbar) {
            items(in: .view)
        }
    }

    /// Actions the system already puts in the menu, which must therefore not be
    /// rendered again.
    ///
    /// Not a style preference — a crash. UIKit's menu builder raises when two
    /// items claim one key equivalent, so shipping our own ⌘A next to the
    /// standard Edit menu's, or our own ⌃⌘S next to the one
    /// `NavigationSplitView` installs, aborts the app the first time anything
    /// enumerates key commands. On iPad that is the moment a hardware keyboard
    /// is attached.
    ///
    /// They stay in the catalog because the catalog describes the app's whole
    /// keyboard surface, and the tests that check for collisions need to see
    /// them. What changes here is only who draws them.
    private static let systemProvided: Set<NavigationAction> = [
        .selectAll,
        .toggleSidebar,
    ]

    @ViewBuilder
    private func items(in placement: CommandPlacement) -> some View {
        let rendered = NavigationCommand.all.filter {
            $0.placement == placement && !Self.systemProvided.contains($0.action)
        }
        ForEach(rendered) { command in
            item(command)
        }
    }

    private func item(_ command: NavigationCommand) -> some View {
        Button(LocalizedStringKey(command.titleKey), systemImage: command.systemImage) {
            _ = router?.perform(command.action)
        }
        .keyboardShortcut(command.shortcut)
        .disabled(!canPerform(command.action))
    }

    /// Whether firing this item would do anything.
    ///
    /// Asked of the router rather than assumed, so an item the router declines
    /// greys out instead of silently swallowing the keystroke — the failure the
    /// notification stopgap had, made visible.
    private func canPerform(_ action: NavigationAction) -> Bool {
        guard let router else { return false }
        return router.accepts(action)
    }
}

// MARK: - Bridging the catalog to SwiftUI

private extension View {
    /// Apply a catalog shortcut, or none.
    @ViewBuilder
    func keyboardShortcut(_ shortcut: CommandShortcut?) -> some View {
        if let shortcut {
            keyboardShortcut(
                KeyEquivalent(shortcut.key),
                modifiers: EventModifiers(shortcut.modifiers)
            )
        } else {
            self
        }
    }
}

private extension KeyEquivalent {
    /// The catalog spells arrows as cases because `CapsuleNavigation` may not
    /// import SwiftUI; this is where they become key equivalents.
    init(_ key: CommandKey) {
        switch key {
        case let .character(character): self.init(character)
        case .leftArrow: self = .leftArrow
        case .rightArrow: self = .rightArrow
        }
    }
}

private extension EventModifiers {
    init(_ modifiers: CommandModifiers) {
        var result: EventModifiers = []
        if modifiers.contains(.command) { result.insert(.command) }
        if modifiers.contains(.shift) { result.insert(.shift) }
        if modifiers.contains(.option) { result.insert(.option) }
        if modifiers.contains(.control) { result.insert(.control) }
        self = result
    }
}
