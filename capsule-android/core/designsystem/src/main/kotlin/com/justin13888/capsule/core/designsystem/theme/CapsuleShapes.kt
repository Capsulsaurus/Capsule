package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

/**
 * The Capsule shape scale.
 *
 * Every step is one or two dp softer than the Material baseline. That is deliberate:
 * this app is a frame around rectangular photographs, and a soft frame reads as chrome
 * while a crisp one competes with the image inside it. The scale stays monotonic, so a
 * component that asks for `medium` is always rounder than one that asks for `small` —
 * the property the wrappers below rely on when they pick a shape by prominence.
 */
val CapsuleShapes =
    Shapes(
        extraSmall = RoundedCornerShape(6.dp),
        small = RoundedCornerShape(10.dp),
        medium = RoundedCornerShape(16.dp),
        large = RoundedCornerShape(22.dp),
        extraLarge = RoundedCornerShape(30.dp),
        largeIncreased = RoundedCornerShape(26.dp),
        extraLargeIncreased = RoundedCornerShape(34.dp),
        extraExtraLarge = RoundedCornerShape(44.dp),
    )
