package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.core.model.LocalAuthGrant
import java.time.Duration

/** Test handle for one import source's row: `settings.scope:<locator>`. */
fun settingsScopeTag(locator: String): String = "settings.scope:$locator"

/** Test handle for one album offered as a source's destination: `settings.scope-album:<id>`. */
fun settingsScopeAlbumTag(albumId: String): String = "settings.scope-album:$albumId"

/**
 * Import sources, trash retention, and how long a confirmed view stays open.
 *
 * The import-source list is the scope-mapping table from the organization design, rendered.
 * Each source is shown by its **relative path** — `DCIM/Camera`, `Pictures/Screenshots` — and
 * never by a MediaStore bucket id, because a bucket id is a hash of the display name that
 * differs across devices and OS versions: two phones pointed at the same folder have to
 * compute the same scope, and a bucket id would give them two.
 *
 * A source with no recorded destination says it will ask, rather than showing the album the
 * default pointer happens to resolve to today. The two are different promises, and the
 * planner records which rule fired precisely so a surprising destination stays explainable.
 */
@Composable
internal fun ColumnScope.LibrarySettingsPage(
    state: SettingsUiState,
    actions: SettingsActions,
) {
    CapsuleSectionHeader(title = stringResource(R.string.android_settings_library_scopes))
    SettingsNote(stringResource(R.string.android_settings_library_scopes_summary))
    if (state.scopes.isEmpty()) {
        SettingsNote(stringResource(R.string.android_settings_library_scopes_empty))
    } else {
        state.scopes.forEach { mapping -> ScopeRow(mapping, state, actions) }
    }
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = RETENTION_GROUP,
        title = stringResource(R.string.android_settings_library_retention),
        summary = stringResource(R.string.android_settings_library_retention_summary),
        options = retentionOptions(),
        selected = state.library.defaultRetention,
        onSelect = actions.onDefaultRetention,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = GRACE_GROUP,
        title = stringResource(R.string.android_settings_library_grace),
        summary = stringResource(R.string.android_settings_library_grace_summary),
        options = graceOptions(),
        selected = state.library.gatedViewGrace,
        onSelect = actions.onGatedViewGrace,
    )
}

/**
 * One import source, and the album its photos land in.
 *
 * The album choice expands under the row rather than opening a sheet: a sheet over a settings
 * list hides the very row being changed, and this decision is only meaningful next to the
 * source it applies to.
 */
@Composable
private fun ColumnScope.ScopeRow(
    mapping: ScopeMapping,
    state: SettingsUiState,
    actions: SettingsActions,
) {
    val expanded = state.editingScope?.locator == mapping.scope.locator
    val destination =
        mapping.destination?.let { albumLabel(it) }
            ?: stringResource(R.string.android_settings_library_scope_unmapped)
    CapsuleListItem(
        // The locator is the source's identity and is never translated: it is a path.
        headline = mapping.scope.locator,
        modifier = Modifier.fillMaxWidth().testTag(settingsScopeTag(mapping.scope.locator)),
        supporting = destination,
        overline = sourceKindLabel(mapping.scope),
        onClick = { actions.onEditScope(if (expanded) null else mapping.scope) },
        contentDescription =
            listOf(
                stringResource(R.string.android_settings_library_scope_change),
                mapping.scope.locator,
                destination,
            ).joinToString(SEPARATOR),
    )
    if (expanded) {
        state.albums.forEach { album -> ScopeAlbumRow(mapping, album, actions) }
    }
}

@Composable
private fun ScopeAlbumRow(
    mapping: ScopeMapping,
    album: AlbumChoice,
    actions: SettingsActions,
) {
    val selected = mapping.destination?.id == album.id
    CapsuleListItem(
        headline = albumLabel(album),
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(start = SettingsInset)
                .testTag(settingsScopeAlbumTag(album.id.value))
                .selectable(
                    selected = selected,
                    role = Role.RadioButton,
                    onClick = { actions.onScopeDestination(mapping.scope, album.id) },
                ),
        trailing = if (selected) ({ Icon(Icons.Filled.Check, contentDescription = null) }) else null,
    )
}

@Composable
private fun retentionOptions(): List<SettingsChoice<Duration>> =
    RETENTION_DAYS.map { days ->
        SettingsChoice(Duration.ofDays(days), daysLabel(days), days.toString())
    }

@Composable
private fun graceOptions(): List<SettingsChoice<Duration>> =
    GRACE_MINUTES.map { minutes ->
        SettingsChoice(Duration.ofMinutes(minutes), minutesLabel(minutes), minutes.toString())
    }

/** The retention windows offered, with the design's own default among them. */
private val RETENTION_DAYS: List<Long> =
    listOf(7L, DeletionState.DEFAULT_RETENTION.toDays(), 60L, 90L).distinct().sorted()

/** The grace windows offered, with SR1's own default among them. */
private val GRACE_MINUTES: List<Long> =
    listOf(1L, LocalAuthGrant.DEFAULT_WINDOW.toMinutes(), 15L, 30L).distinct().sorted()

private const val RETENTION_GROUP = "retention"
private const val GRACE_GROUP = "grace"
