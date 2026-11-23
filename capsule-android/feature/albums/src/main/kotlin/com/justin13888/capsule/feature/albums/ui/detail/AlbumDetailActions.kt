package com.justin13888.capsule.feature.albums.ui.detail

import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.UserId

/**
 * Everything a screen can do to one album, gathered behind one callback bag.
 *
 * A single parameter object rather than eleven lambdas, because album detail really does own
 * eleven distinct actions and a call site listing them one per line stops being readable long
 * before the list stops growing. Grouping also makes the preview and the tests construct one
 * thing instead of remembering which of the eleven they left out.
 */
data class AlbumDetailActions(
    val onBack: () -> Unit = {},
    val onOpenAsset: (AssetId) -> Unit = {},
    val onRename: (String) -> Unit = {},
    val onDelete: () -> Unit = {},
    val onMakeDefault: () -> Unit = {},
    val onInvite: (String, AlbumCapability) -> Unit = { _, _ -> },
    val onRemoveMember: (UserId) -> Unit = {},
    val onSetCapability: (UserId, AlbumCapability) -> Unit = { _, _ -> },
    val onStartUpgrade: () -> Unit = {},
    val onAdvanceUpgrade: () -> Unit = {},
    val onAbortUpgrade: () -> Unit = {},
    val onDismissMessage: () -> Unit = {},
    val onRetry: () -> Unit = {},
)
