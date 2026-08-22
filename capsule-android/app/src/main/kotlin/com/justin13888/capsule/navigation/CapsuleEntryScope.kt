package com.justin13888.capsule.navigation

import androidx.compose.runtime.Stable
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.security.LocalAuthPrompt

/**
 * Everything a destination's content is allowed to reach for, in one object.
 *
 * This is the seam a feature module is filled in through. An entry lambda receives this, and
 * a real screen is wired by turning its fields into the callbacks that screen already
 * declares — `onOpenAsset = { scope.navigator.goTo(AssetViewer(it)) }`. Nothing in a feature
 * module ever imports this class; the app module is where the two meet, which is the same
 * rule the feature convention plugin states for module dependencies.
 *
 * Passed explicitly rather than through a composition local because the entry builder runs
 * *outside* composition — a local would not be readable where the entries are declared.
 */
@Stable
class CapsuleEntryScope(
    /** Moving between destinations. The only navigation API a screen sees. */
    val navigator: CapsuleNavigator,
    /** Fresh-local-auth grants for the gated views. */
    val gate: LocalAuthGate,
    /** The platform authenticator behind the gate. */
    val prompt: LocalAuthPrompt,
)
