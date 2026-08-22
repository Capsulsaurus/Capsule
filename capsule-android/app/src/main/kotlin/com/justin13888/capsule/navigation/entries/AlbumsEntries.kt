package com.justin13888.capsule.navigation.entries

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination
import com.justin13888.capsule.placeholder.PlaceholderScreen
import com.justin13888.capsule.shell.panes.CapsuleListDetailPane

/** Test handle for the Collections index, the list pane of the collections list-detail. */
const val COLLECTIONS_INDEX_TAG: String = "shell.collections-index"

/**
 * Albums, smart albums, and the utility views the Collections tab gathers.
 *
 * Collections is the shell's list-detail case: an index of sections on the left and whichever
 * one is open on the right. It is wired here rather than left to the feature module because
 * the *index* is shell furniture — it names destinations from four different feature modules,
 * so no single feature can own it.
 *
 * Filled in by `:capsule-android:feature:albums`, plus the modules behind the sections it
 * lists.
 */
fun EntryProviderScope<NavKey>.albumsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Collections>(scope) { CollectionsPane(scope) }
    destination<CapsuleDestination.AlbumDetail>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.StackDetail>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SmartAlbums>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SmartAlbumBuilder>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Places>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Duplicates>(scope) { scope.Placeholder(it) }
}

/**
 * The sections reachable from the Collections index, in the order the iOS shell shows them.
 *
 * Trash and Hidden are deliberately absent. They carry a `gatedView`, and the gate is applied
 * by the entry provider — opening one inside a detail pane would render it without ever
 * passing through its own entry, and so without the local-auth gate. Routing them through the
 * back stack instead costs one animation and keeps SR1 impossible to sidestep.
 */
private val indexSections: List<CapsuleDestination> =
    listOf(
        CapsuleDestination.SmartAlbums,
        CapsuleDestination.Places,
        CapsuleDestination.Duplicates,
    )

/** The sections that must be pushed rather than shown in a pane, because they are gated. */
private val gatedSections: List<CapsuleDestination> =
    listOf(
        CapsuleDestination.Trash,
        CapsuleDestination.Hidden,
    )

/** Collections as a list beside its detail, collapsing to one pane on a phone. */
@Composable
private fun CollectionsPane(scope: CapsuleEntryScope) {
    CapsuleListDetailPane(
        listPane = { openDetail -> CollectionsIndex(scope, openDetail) },
        detailPane = { contentKey ->
            val selected = indexSections.firstOrNull { it.contentKey() == contentKey }
            PlaceholderScreen(
                destination = selected ?: CapsuleDestination.Collections,
                navigator = scope.navigator,
            )
        },
    )
}

/** The index itself: every section of the Collections surface, as rows. */
@Composable
private fun CollectionsIndex(
    scope: CapsuleEntryScope,
    openDetail: (String) -> Unit,
) {
    Column(modifier = Modifier.fillMaxSize().testTag(COLLECTIONS_INDEX_TAG)) {
        CapsuleTopAppBar(title = stringResource(CapsuleDestination.Collections.titleRes))
        Column(modifier = Modifier.verticalScroll(rememberScrollState())) {
            indexSections.forEach { section ->
                IndexRow(section) { openDetail(section.contentKey()) }
            }
            gatedSections.forEach { section ->
                IndexRow(section) { scope.navigator.goTo(section) }
            }
        }
    }
}

/** One row of the index. */
@Composable
private fun IndexRow(
    destination: CapsuleDestination,
    onClick: () -> Unit,
) {
    CapsuleListItem(
        headline = stringResource(destination.titleRes),
        modifier = Modifier.testTag("$COLLECTIONS_INDEX_TAG:${destination::class.simpleName}"),
        onClick = onClick,
        trailing = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) },
    )
}

/**
 * A pane content key for a destination.
 *
 * The pane scaffold persists its selection across configuration changes, so the key has to be
 * saveable — the class name is, and every section in the index is an object, so it is unique.
 */
private fun CapsuleDestination.contentKey(): String = this::class.simpleName.orEmpty()
