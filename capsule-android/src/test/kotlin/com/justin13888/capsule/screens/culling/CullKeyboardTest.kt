package com.justin13888.capsule.screens.culling

import android.view.KeyEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * One key per verb, matched with exact modifiers, and the same table the keyboard
 * shortcuts helper lists.
 */
class CullKeyboardTest {
    @Test
    fun everyVerbHasExactlyOneKey() {
        val verbs = CullKeyboard.bindings.map { it.verb }

        assertEquals(CullVerb.entries.toSet(), verbs.toSet())
        assertEquals(verbs.size, verbs.toSet().size)
    }

    @Test
    fun theBareKeysReachTheirVerbs() {
        assertEquals(CullVerb.PICK, CullKeyboard.verbFor(KeyEvent.KEYCODE_P, 0))
        assertEquals(CullVerb.NEUTRAL, CullKeyboard.verbFor(KeyEvent.KEYCODE_U, 0))
        assertEquals(CullVerb.REJECT, CullKeyboard.verbFor(KeyEvent.KEYCODE_X, 0))
        assertEquals(CullVerb.NEXT, CullKeyboard.verbFor(KeyEvent.KEYCODE_DPAD_RIGHT, 0))
        assertEquals(CullVerb.PREVIOUS, CullKeyboard.verbFor(KeyEvent.KEYCODE_DPAD_LEFT, 0))
        assertEquals(CullVerb.EXPAND_STACK, CullKeyboard.verbFor(KeyEvent.KEYCODE_E, 0))
        assertEquals(CullVerb.COLLAPSE_STACK, CullKeyboard.verbFor(KeyEvent.KEYCODE_C, 0))
    }

    @Test
    fun theSweepNeedsCtrlAndABareBackspaceDoesNothing() {
        val leftCtrl = KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON

        assertEquals(CullVerb.SWEEP, CullKeyboard.verbFor(KeyEvent.KEYCODE_DEL, leftCtrl))
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_DEL, 0))
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_DEL, KeyEvent.META_CTRL_ON or KeyEvent.META_SHIFT_ON))
    }

    @Test
    fun aModifiedVerbKeyIsNotTheVerb() {
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_C, KeyEvent.META_CTRL_ON), "Ctrl+C stays copy")
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_P, KeyEvent.META_SHIFT_ON))
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_X, KeyEvent.META_ALT_ON))
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_DPAD_RIGHT, KeyEvent.META_META_ON))
    }

    @Test
    fun lockStatesDoNotStopAKey() {
        assertEquals(CullVerb.PICK, CullKeyboard.verbFor(KeyEvent.KEYCODE_P, KeyEvent.META_CAPS_LOCK_ON or KeyEvent.META_NUM_LOCK_ON))
    }

    @Test
    fun otherKeysAreNotThePasses() {
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_A, 0))
        assertNull(CullKeyboard.verbFor(KeyEvent.KEYCODE_SPACE, 0))
    }

    @Test
    fun aFirstKeyDownIsAVerb() {
        assertEquals(CullVerb.REJECT, CullKeyboard.verbForEvent(true, 0, KeyEvent.KEYCODE_X, 0))
    }

    @Test
    fun anAutoRepeatIsIgnored() {
        assertNull(CullKeyboard.verbForEvent(true, 1, KeyEvent.KEYCODE_X, 0))
        assertNull(CullKeyboard.verbForEvent(true, 5, KeyEvent.KEYCODE_P, 0))
    }

    @Test
    fun aKeyUpIsIgnored() {
        assertNull(CullKeyboard.verbForEvent(false, 0, KeyEvent.KEYCODE_X, 0))
    }

    @Test
    fun theHelperListsTheKeysOnlyWhileASurfaceIsShowing() {
        assertFalse(CullKeyboard.isShowing)
        CullKeyboard.shown()
        assertTrue(CullKeyboard.isShowing)
        CullKeyboard.hidden()
        CullKeyboard.hidden()
        assertFalse(CullKeyboard.isShowing, "an extra hide does not leave the count below zero")
        CullKeyboard.shown()
        assertTrue(CullKeyboard.isShowing)
        CullKeyboard.hidden()
    }
}
