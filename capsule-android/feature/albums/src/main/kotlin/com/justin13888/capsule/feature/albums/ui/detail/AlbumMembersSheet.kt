package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PersonRemove
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumMember
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.asIsoDate
import com.justin13888.capsule.feature.albums.ui.common.capabilityLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * Who holds keys for this album, and at which tier.
 *
 * The controls are disabled outright for a non-admin rather than hidden, and the reason is
 * spelled out above them. Hiding them would leave a read member wondering why the album they
 * can see is not one they can share; saying "only an album admin can invite or remove
 * members" is the same fact, stated once.
 *
 * Every control here proposes an MLS commit that bumps the album's key epoch. Nothing in this
 * sheet grants anything by itself — a capability *is* which keys were delivered — so the
 * local refusals exist to stop the client proposing a commit that cannot succeed.
 */
@Composable
internal fun AlbumMembersSheet(
    state: AlbumDetailUiState.Content,
    actions: AlbumDetailActions,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(AlbumsTestTags.MEMBERS)) {
        CapsuleSectionHeader(title = stringResource(R.string.albums_detail_members))
        if (!state.isAdmin) {
            CapsuleBanner(
                title = stringResource(R.string.albums_members_admin_only),
                modifier = Modifier.padding(horizontal = GUTTER),
                severity = CapsuleBannerSeverity.Info,
            )
        }
        InviteRow(enabled = state.isAdmin, onInvite = actions.onInvite)
        state.album?.members?.forEach { member ->
            MemberRow(member = member, canEdit = state.isAdmin, actions = actions)
        }
    }
}

@Composable
private fun InviteRow(
    enabled: Boolean,
    onInvite: (String, AlbumCapability) -> Unit,
) {
    var recipient by remember { mutableStateOf("") }
    var capability by remember { mutableStateOf(AlbumCapability.Read) }
    val label = stringResource(R.string.albums_members_invite_placeholder)
    Column(modifier = Modifier.padding(GUTTER)) {
        OutlinedTextField(
            value = recipient,
            onValueChange = { recipient = it },
            label = { Text(text = label) },
            singleLine = true,
            enabled = enabled,
            modifier = Modifier.fillMaxWidth().testTag("${AlbumsTestTags.MEMBERS}:recipient"),
        )
        CapabilityChips(
            selected = capability,
            enabled = enabled,
            testTag = "${AlbumsTestTags.MEMBERS}:invite-role",
            onSelect = { capability = it },
        )
        CapsuleButton(
            onClick = { onInvite(recipient, capability) },
            label = stringResource(R.string.albums_members_invite_submit),
            enabled = enabled,
            modifier = Modifier.testTag("${AlbumsTestTags.MEMBERS}:invite"),
        )
    }
}

@Composable
private fun MemberRow(
    member: AlbumMember,
    canEdit: Boolean,
    actions: AlbumDetailActions,
) {
    val joined = stringResource(R.string.albums_members_joined).withArgs("date" to member.joinedAt.asIsoDate())
    val removeLabel = stringResource(R.string.albums_members_remove_a11y).withArgs("name" to member.userId.value)
    Column(modifier = Modifier.testTag("${AlbumsTestTags.MEMBER}:${member.userId.value}")) {
        CapsuleListItem(
            headline = member.userId.value,
            supporting = joined,
            overline = capabilityLabel(member.capability),
            trailing = {
                IconButton(
                    onClick = { actions.onRemoveMember(member.userId) },
                    enabled = canEdit,
                ) { Icon(Icons.Filled.PersonRemove, contentDescription = removeLabel) }
            },
        )
        CapabilityChips(
            selected = member.capability.knownOrNull,
            enabled = canEdit,
            testTag = "${AlbumsTestTags.MEMBER}:${member.userId.value}:role",
            onSelect = { actions.onSetCapability(member.userId, it) },
        )
    }
}

/** The three tiers, as one-of-three chips. A tier outside the set selects none of them. */
@Composable
private fun CapabilityChips(
    selected: AlbumCapability?,
    enabled: Boolean,
    testTag: String,
    onSelect: (AlbumCapability) -> Unit,
) {
    Row(
        modifier = Modifier.padding(horizontal = GUTTER),
        horizontalArrangement = Arrangement.spacedBy(CHIP_SPACING),
    ) {
        AlbumCapability.entries.forEach { capability ->
            CapsuleChip(
                label = capabilityLabel(capability.asWire()),
                selected = capability == selected,
                onSelectedChange = { onSelect(capability) },
                enabled = enabled,
                modifier = Modifier.testTag("$testTag:${capability.wire}"),
            )
        }
    }
}

private val CHIP_SPACING = GUTTER / 2
