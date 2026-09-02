import SwiftUI

extension View {
    /// Configure a text field for typing a URL: no autocapitalisation, no
    /// autocorrection, a URL keyboard, and a Done return key.
    ///
    /// Everything but autocorrection is a software-keyboard affordance, so none
    /// of it exists on macOS — where the same field is simply typed into with a
    /// hardware keyboard. Bundling the four modifiers behind one intent keeps
    /// the platform split out of the settings form.
    func capsuleURLFieldStyle() -> some View {
        #if os(iOS)
            autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .keyboardType(.URL)
                .submitLabel(.done)
        #else
            autocorrectionDisabled()
        #endif
    }
}
