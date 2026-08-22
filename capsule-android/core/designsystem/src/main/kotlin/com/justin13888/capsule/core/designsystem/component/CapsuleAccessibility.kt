package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The smallest thing a finger can reliably hit.
 *
 * Material's own components stop at 40dp for a small button, which is fine on a phone
 * held in two hands and not fine on a tablet on a light table with cold hands. Every
 * Capsule wrapper floors its interactive area here so that no feature has to remember.
 */
val CapsuleMinimumTouchTarget = 48.dp

/** Applies [CapsuleMinimumTouchTarget] as a floor without capping how large the element can grow. */
fun Modifier.capsuleTouchTarget(): Modifier = defaultMinSize(CapsuleMinimumTouchTarget, CapsuleMinimumTouchTarget)

/**
 * Attaches [description] as the element's accessibility label, or leaves semantics alone
 * when it is `null`.
 *
 * Every wrapper takes an optional description rather than owning one, because the design
 * system has no idea what the button does — only the feature does, and only the feature
 * has a catalog key for it. `null` is the honest default for a control whose visible
 * label already reads correctly to a screen reader.
 */
fun Modifier.capsuleDescription(description: String?): Modifier =
    if (description == null) this else semantics { contentDescription = description }
