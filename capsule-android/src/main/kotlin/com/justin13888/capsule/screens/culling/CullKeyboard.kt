package com.justin13888.capsule.screens.culling

import android.view.KeyEvent
import java.util.concurrent.atomic.AtomicInteger

/** One key the pass answers: [keyCode] pressed with exactly [modifiers] held. */
data class CullBinding(
    val verb: CullVerb,
    val keyCode: Int,
    /** A mask of `KeyEvent.META_*_ON` bits; `0` for a bare key. */
    val modifiers: Int = 0,
)

/**
 * The culling pass's keyboard: one bare key per verdict and per stack move, the arrows to
 * page, and the reject sweep alone behind Ctrl, on Ctrl+Backspace, because it is the one
 * destructive step and a bare key would be one stray press from its confirmation. The same
 * keys as the Apple client's Culling menu, where the sweep is on Command+Backspace.
 *
 * The table is what the surface matches key presses against and what the system keyboard
 * shortcuts helper lists, so the two cannot disagree.
 */
object CullKeyboard {
    /** The modifier bits a binding can name; any other meta state (Caps Lock, Fn) is ignored. */
    const val MODIFIER_MASK: Int =
        KeyEvent.META_CTRL_ON or KeyEvent.META_ALT_ON or KeyEvent.META_SHIFT_ON or KeyEvent.META_META_ON

    /** Every key the pass answers, in the order a pass uses them. */
    val bindings: List<CullBinding> =
        listOf(
            CullBinding(CullVerb.NEXT, KeyEvent.KEYCODE_DPAD_RIGHT),
            CullBinding(CullVerb.PREVIOUS, KeyEvent.KEYCODE_DPAD_LEFT),
            CullBinding(CullVerb.PICK, KeyEvent.KEYCODE_P),
            CullBinding(CullVerb.NEUTRAL, KeyEvent.KEYCODE_U),
            CullBinding(CullVerb.REJECT, KeyEvent.KEYCODE_X),
            CullBinding(CullVerb.EXPAND_STACK, KeyEvent.KEYCODE_E),
            CullBinding(CullVerb.COLLAPSE_STACK, KeyEvent.KEYCODE_C),
            CullBinding(CullVerb.SWEEP, KeyEvent.KEYCODE_DEL, KeyEvent.META_CTRL_ON),
        )

    /**
     * The verb [keyCode] asks for with [metaState] held, or `null` when the pass does not
     * answer it. Modifiers must match exactly, so Ctrl+C is never a collapse.
     */
    fun verbFor(
        keyCode: Int,
        metaState: Int,
    ): CullVerb? {
        val held = normalized(metaState)
        return bindings.firstOrNull { it.keyCode == keyCode && it.modifiers == held }?.verb
    }

    /** [metaState] reduced to the `META_*_ON` bits a binding names, left and right alike. */
    fun normalized(metaState: Int): Int = metaState and MODIFIER_MASK

    /**
     * How many culling surfaces are on screen. The activity lists the culling keys in the
     * keyboard shortcuts helper only while one is.
     */
    private val showing = AtomicInteger(0)

    /** Whether a culling surface is on screen. */
    val isShowing: Boolean get() = showing.get() > 0

    /** A culling surface appeared; pair with [hidden]. */
    fun shown() {
        showing.incrementAndGet()
    }

    /** A culling surface left the screen. */
    fun hidden() {
        showing.updateAndGet { maxOf(it - 1, 0) }
    }
}
