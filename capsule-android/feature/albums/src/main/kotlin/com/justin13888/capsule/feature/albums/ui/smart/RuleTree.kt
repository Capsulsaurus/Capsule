package com.justin13888.capsule.feature.albums.ui.smart

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.DataArray
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.feature.albums.builder.DraftCondition
import com.justin13888.capsule.feature.albums.builder.DraftGroup
import com.justin13888.capsule.feature.albums.builder.DraftNode
import com.justin13888.capsule.feature.albums.builder.DraftNodeId
import com.justin13888.capsule.feature.albums.builder.DraftOperand
import com.justin13888.capsule.feature.albums.builder.GroupKind
import com.justin13888.capsule.feature.albums.builder.PredicateEditing
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.PickerButton
import com.justin13888.capsule.feature.albums.ui.common.fieldLabel
import com.justin13888.capsule.feature.albums.ui.common.operatorLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/** Every edit one row of the rule tree can make. */
data class RuleTreeActions(
    val onAddCondition: (DraftNodeId) -> Unit = {},
    val onAddGroup: (DraftNodeId, GroupKind) -> Unit = { _, _ -> },
    val onRemove: (DraftNodeId) -> Unit = {},
    val onNegate: (DraftNodeId, Boolean) -> Unit = { _, _ -> },
    val onGroupKind: (DraftNodeId, GroupKind) -> Unit = { _, _ -> },
    val onField: (DraftNodeId, QueryField) -> Unit = { _, _ -> },
    val onOperator: (DraftNodeId, Operator) -> Unit = { _, _ -> },
    val onOperand: (DraftNodeId, DraftOperand) -> Unit = { _, _ -> },
    val onModelSlot: (DraftNodeId, ModelSlot) -> Unit = { _, _ -> },
)

/** One node of the rule tree, flattened with the depth it should be indented to. */
internal data class RuleRow(
    val node: DraftNode,
    val depth: Int,
    /** Whether this node is the root, which cannot be removed or negated away. */
    val isRoot: Boolean,
)

/**
 * Flattens the tree into rows.
 *
 * A flat list rather than a nested composable tree, so the whole rule can live in one
 * `LazyColumn`: a 64-condition rule nested eight deep is a tall surface, and a recursive
 * `Column` would compose all of it at once.
 */
internal fun flattenRule(
    node: DraftNode,
    depth: Int = 0,
    isRoot: Boolean = true,
): List<RuleRow> =
    buildList {
        add(RuleRow(node, depth, isRoot))
        if (node is DraftGroup) {
            node.children.forEach { child -> addAll(flattenRule(child, depth + 1, isRoot = false)) }
        }
    }

/** One row of the rule tree, indented to its depth. */
@Composable
internal fun RuleNodeRow(
    row: RuleRow,
    position: Int,
    actions: RuleTreeActions,
    modifier: Modifier = Modifier,
) {
    val tag = "${AlbumsTestTags.BUILDER_NODE}:${row.node.id.value}"
    val announced =
        when (row.node) {
            is DraftGroup -> stringResource(R.string.albums_builder_group_a11y).withArgs("position" to position)
            is DraftCondition -> stringResource(R.string.albums_builder_condition_a11y).withArgs("position" to position)
        }
    Column(
        modifier =
            modifier
                .padding(start = INDENT * row.depth, top = ROW_SPACING / 2, bottom = ROW_SPACING / 2)
                .testTag(tag)
                .semantics { contentDescription = announced },
    ) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(ROW_SPACING)) {
            NegateChip(row, actions, tag)
            when (val node = row.node) {
                is DraftGroup -> GroupControls(node, actions, tag)
                is DraftCondition -> ConditionControls(node, actions, tag)
            }
            if (!row.isRoot) {
                IconButton(
                    onClick = { actions.onRemove(row.node.id) },
                    modifier = Modifier.testTag("$tag:remove"),
                ) { Icon(Icons.Filled.Delete, contentDescription = stringResource(R.string.albums_builder_remove)) }
            }
        }
    }
}

@Composable
private fun NegateChip(
    row: RuleRow,
    actions: RuleTreeActions,
    tag: String,
) {
    CapsuleChip(
        label = stringResource(R.string.albums_builder_negate),
        selected = row.node.negated,
        onSelectedChange = { actions.onNegate(row.node.id, it) },
        modifier = Modifier.testTag("$tag:negate"),
    )
}

@Composable
private fun GroupControls(
    node: DraftGroup,
    actions: RuleTreeActions,
    tag: String,
) {
    val all = stringResource(R.string.albums_builder_group_all)
    val any = stringResource(R.string.albums_builder_group_any)
    CapsuleChip(
        label = all,
        selected = node.kind == GroupKind.All,
        onSelectedChange = { actions.onGroupKind(node.id, GroupKind.All) },
        modifier = Modifier.testTag("$tag:all"),
    )
    CapsuleChip(
        label = any,
        selected = node.kind == GroupKind.Any,
        onSelectedChange = { actions.onGroupKind(node.id, GroupKind.Any) },
        modifier = Modifier.testTag("$tag:any"),
    )
    IconButton(
        onClick = { actions.onAddCondition(node.id) },
        modifier = Modifier.testTag("$tag:add-condition"),
    ) { Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.albums_builder_add_condition)) }
    IconButton(
        onClick = { actions.onAddGroup(node.id, GroupKind.All) },
        modifier = Modifier.testTag("$tag:add-group"),
    ) { Icon(Icons.Filled.DataArray, contentDescription = stringResource(R.string.albums_builder_add_group)) }
}

@Composable
private fun ConditionControls(
    node: DraftCondition,
    actions: RuleTreeActions,
    tag: String,
) {
    PickerButton(
        label = fieldLabel(node.field),
        options = QueryField.entries,
        optionLabel = { fieldLabel(it) },
        onSelect = { actions.onField(node.id, it) },
        testTag = "$tag:field",
    )
    PickerButton(
        label = operatorLabel(node.op),
        // Only the operators legal for this field's type class are offered, so an illegal
        // pairing is unreachable rather than merely refused after the fact.
        options = PredicateEditing.operatorsFor(node.field),
        optionLabel = { operatorLabel(it) },
        onSelect = { actions.onOperator(node.id, it) },
        testTag = "$tag:operator",
    )
    OperandEditor(
        field = node.field,
        operand = node.operand,
        onChange = { actions.onOperand(node.id, it) },
        testTag = tag,
    )
    node.modelSlot?.let { slot -> ModelSlotChip(slot, tag) }
}

/**
 * The model slot an AI-derived condition queries.
 *
 * Shown rather than editable: a term over `tags_ai` or `people_cluster` is only meaningful
 * against the slot its values were produced by, and letting the user point it at an arbitrary
 * one would invite exactly the cross-version comparison the staleness rule forbids.
 */
@Composable
private fun ModelSlotChip(
    slot: ModelSlot,
    tag: String,
) {
    val label = stringResource(R.string.albums_builder_model)
    CapsuleChip(
        label = "$label: ${slot.modelId.value}",
        selected = true,
        onSelectedChange = {},
        enabled = false,
        modifier = Modifier.testTag("$tag:model"),
    )
}

private val INDENT = 16.dp
private val ROW_SPACING = 8.dp
