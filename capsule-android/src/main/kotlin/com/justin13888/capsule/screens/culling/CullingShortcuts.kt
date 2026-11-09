package com.justin13888.capsule.screens.culling

import android.content.Context
import android.view.KeyboardShortcutGroup
import android.view.KeyboardShortcutInfo
import androidx.annotation.StringRes
import com.justin13888.capsule.R

/** The name every door shows for [this] verb: the button, the TalkBack action and the key. */
@get:StringRes
val CullVerb.label: Int
    get() =
        when (this) {
            CullVerb.PICK -> R.string.app_culling_action_pick
            CullVerb.NEUTRAL -> R.string.app_culling_action_unflag
            CullVerb.REJECT -> R.string.app_culling_action_reject
            CullVerb.NEXT -> R.string.app_culling_action_next
            CullVerb.PREVIOUS -> R.string.app_culling_action_previous
            CullVerb.EXPAND_STACK -> R.string.app_culling_action_expand_stack
            CullVerb.COLLAPSE_STACK -> R.string.app_culling_action_collapse_stack
            CullVerb.SWEEP -> R.string.app_culling_action_sweep
        }

/**
 * The culling keys as the system keyboard shortcuts helper lists them (Meta+/ or the
 * activity's `requestShowKeyboardShortcuts`), built from [CullKeyboard.bindings].
 */
fun CullKeyboard.shortcutGroup(context: Context): KeyboardShortcutGroup =
    KeyboardShortcutGroup(
        context.getString(R.string.app_culling_title),
        bindings.map { KeyboardShortcutInfo(context.getString(it.verb.label), it.keyCode, it.modifiers) },
    )
