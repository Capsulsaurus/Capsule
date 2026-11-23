package com.justin13888.capsule.feature.albums.ui.smart

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.feature.albums.builder.DraftOperand
import com.justin13888.capsule.feature.albums.ui.common.PickerButton

/**
 * The controls for one condition's literal, chosen by the `(field, op)` pair.
 *
 * There is one branch per operand shape and no free-text fallback, which is the point: the
 * grammar fixes the shape, so the editor renders the controls for *that* shape or it renders
 * nothing. A generic text box would let a user type a date into a rating and discover the
 * problem only when the validator refused the save.
 */
@Composable
internal fun OperandEditor(
    field: QueryField,
    operand: DraftOperand,
    onChange: (DraftOperand) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    when (operand) {
        is DraftOperand.TemporalPoint ->
            ValueField(operand.text, { onChange(DraftOperand.TemporalPoint(it)) }, "$testTag:at", modifier)

        is DraftOperand.Number ->
            ValueField(operand.text, { onChange(DraftOperand.Number(it)) }, "$testTag:number", modifier)

        is DraftOperand.TemporalRange ->
            RangeFields(
                from = operand.fromText,
                to = operand.toText,
                onChange = { from, to -> onChange(DraftOperand.TemporalRange(from, to)) },
                testTag = testTag,
                modifier = modifier,
            )

        is DraftOperand.NumberRange ->
            RangeFields(
                from = operand.fromText,
                to = operand.toText,
                onChange = { from, to -> onChange(DraftOperand.NumberRange(from, to)) },
                testTag = testTag,
                modifier = modifier,
            )

        is DraftOperand.EnumValue ->
            PickerButton(
                label = operand.raw,
                options = DraftOperand.closedValues(field),
                optionLabel = { it },
                onSelect = { onChange(DraftOperand.EnumValue(it)) },
                testTag = "$testTag:enum",
                modifier = modifier,
            )

        is DraftOperand.EnumSet ->
            EnumSetChips(field, operand, onChange, "$testTag:enum-set", modifier)

        is DraftOperand.Cull ->
            PickerButton(
                label = cullLabel(operand.flag),
                options = CullFlag.entries,
                optionLabel = { cullLabel(it) },
                onSelect = { onChange(DraftOperand.Cull(it)) },
                testTag = "$testTag:cull",
                modifier = modifier,
            )

        is DraftOperand.Bool -> BoolChips(operand.value, { onChange(DraftOperand.Bool(it)) }, "$testTag:bool", modifier)

        is DraftOperand.Presence ->
            BoolChips(operand.mustExist, { onChange(DraftOperand.Presence(it)) }, "$testTag:presence", modifier)

        is DraftOperand.Strings -> StringSetEditor(operand, onChange, "$testTag:strings", modifier)
    }
}

@Composable
private fun ValueField(
    value: String,
    onChange: (String) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    val label = stringResource(R.string.albums_builder_value)
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(text = label) },
        singleLine = true,
        modifier = modifier.width(FIELD_WIDTH).testTag(testTag),
    )
}

@Composable
private fun RangeFields(
    from: String,
    to: String,
    onChange: (String, String) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    val fromLabel = stringResource(R.string.albums_builder_value_from)
    val toLabel = stringResource(R.string.albums_builder_value_to)
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(SPACING)) {
        OutlinedTextField(
            value = from,
            onValueChange = { onChange(it, to) },
            label = { Text(text = fromLabel) },
            singleLine = true,
            modifier = Modifier.width(FIELD_WIDTH).testTag("$testTag:from"),
        )
        OutlinedTextField(
            value = to,
            onValueChange = { onChange(from, it) },
            label = { Text(text = toLabel) },
            singleLine = true,
            modifier = Modifier.width(FIELD_WIDTH).testTag("$testTag:to"),
        )
    }
}

@Composable
private fun EnumSetChips(
    field: QueryField,
    operand: DraftOperand.EnumSet,
    onChange: (DraftOperand) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(SPACING)) {
        DraftOperand.closedValues(field).forEach { value ->
            val selected = value in operand.raw
            CapsuleChip(
                label = value,
                selected = selected,
                onSelectedChange = { checked ->
                    val next = if (checked) operand.raw + value else operand.raw - value
                    onChange(DraftOperand.EnumSet(next.distinct()))
                },
                modifier = Modifier.testTag("$testTag:$value"),
            )
        }
    }
}

@Composable
private fun BoolChips(
    value: Boolean,
    onChange: (Boolean) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    val yes = stringResource(R.string.albums_builder_value_true)
    val no = stringResource(R.string.albums_builder_value_false)
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(SPACING)) {
        CapsuleChip(
            label = yes,
            selected = value,
            onSelectedChange = { onChange(true) },
            modifier = Modifier.testTag("$testTag:true"),
        )
        CapsuleChip(
            label = no,
            selected = !value,
            onSelectedChange = { onChange(false) },
            modifier = Modifier.testTag("$testTag:false"),
        )
    }
}

/**
 * A bounded list of strings or ids.
 *
 * Adding is refused by the editor past 64 members and each entry past 256 bytes, so the
 * control below can stay a plain list: the bound lives in one place rather than in every
 * widget that can grow one.
 */
@Composable
private fun StringSetEditor(
    operand: DraftOperand.Strings,
    onChange: (DraftOperand) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
) {
    val label = stringResource(R.string.albums_builder_value)
    val add = stringResource(R.string.albums_builder_add_value)
    val removeValue = stringResource(R.string.albums_builder_remove_value)
    Column(modifier = modifier) {
        operand.members.forEachIndexed { index, member ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = member,
                    onValueChange = { text ->
                        onChange(DraftOperand.Strings(operand.members.toMutableList().also { it[index] = text }))
                    },
                    label = { Text(text = label) },
                    singleLine = true,
                    modifier = Modifier.width(FIELD_WIDTH).testTag("$testTag:$index"),
                )
                IconButton(
                    onClick = {
                        onChange(DraftOperand.Strings(operand.members.filterIndexed { at, _ -> at != index }))
                    },
                    modifier = Modifier.testTag("$testTag:$index:remove"),
                ) { Icon(Icons.Filled.Close, contentDescription = removeValue) }
            }
        }
        IconButton(
            onClick = { onChange(DraftOperand.Strings(operand.members + "")) },
            modifier = Modifier.testTag("$testTag:add"),
        ) { Icon(Icons.Filled.Add, contentDescription = add) }
    }
}

/** The name of one culling flag. */
@Composable
private fun cullLabel(flag: CullFlag): String =
    stringResource(
        when (flag) {
            CullFlag.Pick -> R.string.albums_cull_pick
            CullFlag.Neutral -> R.string.albums_cull_neutral
            CullFlag.Reject -> R.string.albums_cull_reject
        },
    )

private val FIELD_WIDTH = 160.dp
private val SPACING = 8.dp
