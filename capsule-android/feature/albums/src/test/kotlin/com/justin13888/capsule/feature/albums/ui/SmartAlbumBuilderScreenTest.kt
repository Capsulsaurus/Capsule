package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.PredicateViolation
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.feature.albums.builder.DraftCondition
import com.justin13888.capsule.feature.albums.builder.DraftGroup
import com.justin13888.capsule.feature.albums.builder.DraftNodeId
import com.justin13888.capsule.feature.albums.builder.DraftOperand
import com.justin13888.capsule.feature.albums.builder.PredicateDraft
import com.justin13888.capsule.feature.albums.data.SampledMatchCount
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.smart.RuleTreeActions
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderScreen
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderUiState
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The builder puts the three things that decide a rule on screen at once.
 *
 * The live count, the complete list of structural rejections, and whether saving is possible.
 * A rule that reads correctly and matches nothing is the common mistake, and a rejection list
 * showing only the first problem forces a user to fix a synced definition one error at a
 * time — which is exactly what they cannot do when the definition came from another device.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SmartAlbumBuilderScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val condition =
        DraftCondition(
            id = DraftNodeId(1),
            field = QueryField.Rating,
            op = Operator.Gte,
            operand = DraftOperand.Number("4"),
        )

    private val draft =
        PredicateDraft(root = DraftGroup(id = DraftNodeId(0), children = listOf(condition)), nextId = 2)

    private fun setContent(
        state: SmartAlbumBuilderUiState,
        actions: RuleTreeActions = RuleTreeActions(),
        onSave: () -> Unit = {},
    ) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                SmartAlbumBuilderScreen(
                    state = state,
                    actions = actions,
                    onBack = {},
                    onName = {},
                    onSortKey = {},
                    onSortDirection = {},
                    onSave = onSave,
                    onDismissRefusal = {},
                )
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `the live match count is shown beside the rule`() {
        setContent(
            SmartAlbumBuilderUiState(
                name = "Keepers",
                draft = draft,
                matchCount = SampledMatchCount(matches = 42, sampled = 100, isComplete = true),
            ),
        )
        compose.onNodeWithTag(AlbumsTestTags.BUILDER_COUNT).assertIsDisplayed()
        compose.onNodeWithText("42 photos match").assertIsDisplayed()
    }

    @Test
    fun `a sampled count says so rather than passing itself off as exact`() {
        setContent(
            SmartAlbumBuilderUiState(
                name = "Keepers",
                draft = draft,
                matchCount = SampledMatchCount(matches = 42, sampled = 4_096, isComplete = false),
            ),
        )
        compose.onNodeWithText("42 photos match of the 4096 most recent photos").assertIsDisplayed()
    }

    @Test
    fun `every structural rejection is listed, not just the first`() {
        setContent(
            SmartAlbumBuilderUiState(
                name = "Broken",
                draft = draft,
                violations =
                    persistentListOf(
                        PredicateViolation.DepthExceeded(actual = 9),
                        PredicateViolation.TermCountExceeded(actual = 70),
                    ),
            ),
        )
        compose.onNodeWithTag(AlbumsTestTags.BUILDER_VIOLATIONS).assertIsDisplayed()
        compose.onNodeWithText("This rule nests 9 groups deep; the limit is 8.").assertIsDisplayed()
        compose.onNodeWithText("This rule holds 70 conditions; the limit is 64.").assertIsDisplayed()
    }

    @Test
    fun `saving is refused while a rejection stands`() {
        var saved = 0
        setContent(
            SmartAlbumBuilderUiState(
                name = "Broken",
                draft = draft,
                violations = persistentListOf(PredicateViolation.DepthExceeded(actual = 9)),
            ),
            onSave = { saved++ },
        )
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER}:save").assertIsNotEnabled()
        assertEquals(0, saved)
    }

    @Test
    fun `a well formed, named rule can be saved`() {
        var saved = 0
        setContent(SmartAlbumBuilderUiState(name = "Keepers", draft = draft), onSave = { saved++ })
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER}:save").assertIsEnabled().performClick()
        compose.waitForIdle()
        assertEquals(1, saved)
    }

    @Test
    fun `a condition renders its field, its comparison and its value`() {
        setContent(SmartAlbumBuilderUiState(name = "Keepers", draft = draft))
        val tag = "${AlbumsTestTags.BUILDER_NODE}:1"
        compose.onNodeWithTag("$tag:field").assertIsDisplayed()
        compose.onNodeWithTag("$tag:operator").assertIsDisplayed()
        compose.onNodeWithTag("$tag:number").assertIsDisplayed()
    }

    @Test
    fun `adding a condition asks the group it was pressed on`() {
        var parent: DraftNodeId? = null
        setContent(
            SmartAlbumBuilderUiState(name = "Keepers", draft = draft),
            actions = RuleTreeActions(onAddCondition = { parent = it }),
        )
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER_NODE}:0:add-condition").performClick()
        compose.waitForIdle()
        assertEquals(DraftNodeId(0), parent)
    }

    @Test
    fun `a rule from a newer grammar refuses to open rather than showing it truncated`() {
        setContent(SmartAlbumBuilderUiState(isForwardSchema = true, name = "From another device"))
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER}:forward-schema").assertIsDisplayed()
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER_NODE}:0").assertDoesNotExist()
        compose.onNodeWithTag("${AlbumsTestTags.BUILDER}:save").assertIsNotEnabled()
    }
}
