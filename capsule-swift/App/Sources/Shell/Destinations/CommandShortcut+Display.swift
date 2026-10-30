import CapsuleNavigation

extension CommandShortcut {
    /// The shortcut as the platform prints it in a menu: modifier glyphs in
    /// ⌃⌥⇧⌘ order, then the key, letters upper-cased.
    ///
    /// For the in-app shortcut list, which is the only place a phone can show
    /// its keyboard bindings. The glyphs are the platform's own notation, the
    /// same in every language, so nothing here is translatable.
    var displayText: String {
        var text = ""
        if modifiers.contains(.control) { text += "⌃" }
        if modifiers.contains(.option) { text += "⌥" }
        if modifiers.contains(.shift) { text += "⇧" }
        if modifiers.contains(.command) { text += "⌘" }
        switch key {
        case let .character(character): text += String(character).uppercased()
        case .leftArrow: text += "←"
        case .rightArrow: text += "→"
        case .delete: text += "⌫"
        }
        return text
    }
}
