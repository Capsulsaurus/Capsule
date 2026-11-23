package com.justin13888.capsule.feature.albums.ui.smart

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.LwwStamp
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SmartAlbumValidation
import com.justin13888.capsule.core.model.SmartAlbumValidator
import com.justin13888.capsule.core.model.SortDirection
import com.justin13888.capsule.core.model.SortKey
import com.justin13888.capsule.core.model.SortSpec
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.builder.DraftCompilation
import com.justin13888.capsule.feature.albums.builder.DraftEdit
import com.justin13888.capsule.feature.albums.builder.DraftNodeId
import com.justin13888.capsule.feature.albums.builder.DraftOperand
import com.justin13888.capsule.feature.albums.builder.GroupKind
import com.justin13888.capsule.feature.albums.builder.PredicateCompiler
import com.justin13888.capsule.feature.albums.builder.PredicateDraft
import com.justin13888.capsule.feature.albums.builder.PredicateEditing
import com.justin13888.capsule.feature.albums.data.SmartAlbumPreview
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Instant

/**
 * The smart-album builder: an editor over the closed predicate grammar.
 *
 * Every edit goes through [PredicateEditing], which refuses an illegal one outright, and
 * every resulting rule goes through [SmartAlbumValidator], which reports *every* structural
 * rejection rather than the first. Two gates rather than one, because they catch different
 * things: the editor stops the user building an out-of-bounds tree, and the validator is the
 * same gate a definition arriving over sync has to pass — running it here means the builder
 * cannot produce something another device would refuse.
 *
 * Nothing is ever silently coerced. A refused edit leaves the rule exactly as it was and
 * says why.
 */
@Suppress("TooManyFunctions", "LongParameterList")
class SmartAlbumBuilderViewModel(
    private val smartAlbumId: SmartAlbumId?,
    private val search: SearchRepository,
    private val sessions: SessionRepository,
    private val preview: SmartAlbumPreview,
    private val clock: Clock,
) : ViewModel() {
    private val state = MutableStateFlow(SmartAlbumBuilderUiState(isLoading = smartAlbumId != null))
    private var counting: Job? = null

    /** The rule as it currently stands. */
    val uiState: StateFlow<SmartAlbumBuilderUiState> = state.asStateFlow()

    init {
        if (smartAlbumId == null) revalidate() else load(smartAlbumId)
    }

    /** Renames the smart album. */
    fun setName(name: String) {
        state.value = state.value.copy(name = name)
        revalidate()
    }

    /** Appends a condition under a group. */
    fun addCondition(parent: DraftNodeId) {
        apply(PredicateEditing.addCondition(state.value.draft, parent, clock.instant()))
    }

    /** Appends a nested group under a group. */
    fun addGroup(
        parent: DraftNodeId,
        kind: GroupKind,
    ) = apply(PredicateEditing.addGroup(state.value.draft, parent, kind))

    /** Removes a node and everything under it. */
    fun remove(id: DraftNodeId) = apply(PredicateEditing.remove(state.value.draft, id))

    /** Inverts, or un-inverts, one node. */
    fun setNegated(
        id: DraftNodeId,
        negated: Boolean,
    ) = apply(PredicateEditing.setNegated(state.value.draft, id, negated))

    /** Switches a group between AND and OR. */
    fun setGroupKind(
        id: DraftNodeId,
        kind: GroupKind,
    ) = apply(PredicateEditing.setGroupKind(state.value.draft, id, kind))

    /** Points a condition at a different field, re-seeding its operator and operand. */
    fun setField(
        id: DraftNodeId,
        field: QueryField,
    ) = apply(PredicateEditing.setField(state.value.draft, id, field, clock.instant()))

    /** Changes a condition's operator, refusing one illegal for its field. */
    fun setOperator(
        id: DraftNodeId,
        op: Operator,
    ) = apply(PredicateEditing.setOperator(state.value.draft, id, op, clock.instant()))

    /** Writes a condition's operand. */
    fun setOperand(
        id: DraftNodeId,
        operand: DraftOperand,
    ) = apply(PredicateEditing.setOperand(state.value.draft, id, operand))

    /** Names the model slot an AI-derived condition queries. */
    fun setModelSlot(
        id: DraftNodeId,
        slot: ModelSlot,
    ) = apply(PredicateEditing.setModelSlot(state.value.draft, id, slot))

    /** Changes what the results are ordered by. */
    fun setSortKey(key: SortKey) {
        state.value = state.value.copy(sortKey = key)
    }

    /** Changes which way the results are ordered. */
    fun setSortDirection(direction: SortDirection) {
        state.value = state.value.copy(sortDirection = direction)
    }

    /** Clears the refusal after the screen has shown it. */
    fun dismissRefusal() {
        state.value = state.value.copy(refusal = null)
    }

    /**
     * Writes the definition — one stamped write of the whole thing.
     *
     * Whole-definition rather than field-wise, so two devices editing the same smart album can
     * never merge into a tree neither of them wrote.
     */
    fun save() {
        val current = state.value
        if (!current.canSave) return
        val compiled = PredicateCompiler.compile(current.draft) as? DraftCompilation.Compiled ?: return
        viewModelScope.launch {
            val definition = definition(current, compiled.predicate, stamp())
            runCatching { search.saveSmartAlbum(definition) }
                .onSuccess { state.value = state.value.copy(isSaved = true) }
                .onFailure { failure -> if (failure is CapsuleError) state.value = state.value.copy(isSaved = false) }
        }
    }

    private fun load(id: SmartAlbumId) {
        viewModelScope.launch {
            val definition = search.smartAlbums().first().firstOrNull { it.id == id }
            if (definition == null) {
                state.value = state.value.copy(isLoading = false)
                return@launch
            }
            val forward = SmartAlbumValidator.validate(definition) is SmartAlbumValidation.PreservedUnevaluated
            val draft = if (forward) null else PredicateCompiler.toDraft(definition)
            state.value =
                state.value.copy(
                    isLoading = false,
                    isForwardSchema = forward || draft == null,
                    name = definition.displayName.value.orEmpty(),
                    draft = draft ?: PredicateDraft.empty(),
                    sortKey = definition.sort?.key?.knownOrNull ?: SortKey.CaptureTimestamp,
                    sortDirection = definition.sort?.direction?.knownOrNull ?: SortDirection.Descending,
                )
            revalidate()
        }
    }

    private fun apply(edit: DraftEdit) {
        when (edit) {
            is DraftEdit.Refused -> state.value = state.value.copy(refusal = edit.reason)
            is DraftEdit.Applied -> {
                state.value = state.value.copy(draft = edit.draft, refusal = null)
                revalidate()
            }
        }
    }

    /**
     * Re-runs both gates and the live count.
     *
     * The count is recomputed on every edit rather than on a button, because the number *is*
     * the feedback: a rule that reads correctly and matches nothing is the common failure,
     * and it is only visible if the count moves while the rule is being written.
     */
    private fun revalidate() {
        val current = state.value
        when (val compiled = PredicateCompiler.compile(current.draft)) {
            is DraftCompilation.Incomplete ->
                state.value =
                    current.copy(
                        incomplete = compiled.nodes.toImmutableList(),
                        violations = persistentListOf(),
                        matchCount = null,
                    )

            is DraftCompilation.Compiled -> {
                val validation = SmartAlbumValidator.validate(definition(current, compiled.predicate, STATIC_STAMP))
                state.value =
                    current.copy(
                        incomplete = persistentListOf(),
                        violations =
                            (validation as? SmartAlbumValidation.Rejected)
                                ?.violations
                                .orEmpty()
                                .toImmutableList(),
                    )
                if (validation is SmartAlbumValidation.Evaluable) recount(compiled.predicate)
            }
        }
    }

    private fun recount(predicate: Predicate) {
        counting?.cancel()
        state.value = state.value.copy(isCounting = true)
        counting =
            viewModelScope.launch {
                val counted = runCatching { preview.count(predicate) }.getOrNull()
                state.value = state.value.copy(matchCount = counted, isCounting = false)
            }
    }

    private fun definition(
        current: SmartAlbumBuilderUiState,
        predicate: Predicate,
        stamp: LwwStamp,
    ): SmartAlbumDefinition =
        PredicateCompiler.toDefinition(
            id = smartAlbumId ?: SmartAlbumId(NEW_DEFINITION_ID),
            displayName = current.name,
            predicate = predicate,
            sort = SortSpec(key = current.sortKey.asWire(), direction = current.sortDirection.asWire()),
            stamp = stamp,
        )

    /** The stamp a save carries: this device, now. */
    private suspend fun stamp(): LwwStamp =
        LwwStamp(
            writtenAt = clock.instant(),
            by =
                sessions
                    .devices()
                    .first()
                    .firstOrNull(Device::isThisDevice)
                    ?.id ?: DeviceId(UNKNOWN_DEVICE),
        )

    private companion object {
        /**
         * The id a brand-new definition is written under.
         *
         * Fixed rather than minted here because minting a UUIDv7 is the catalog's job, and a
         * client-side id that later disagreed with the one the catalog assigned would produce
         * two smart albums where the user made one.
         */
        const val NEW_DEFINITION_ID = "00000000-0000-7000-8000-000000000000"

        /** Stands in for the writing device when the directory has not been read yet. */
        const val UNKNOWN_DEVICE = "00000000-0000-4000-8000-000000000000"

        /**
         * A fixed stamp used only while validating.
         *
         * Validation never looks at the stamp — it checks the name's length and the tree's
         * shape — so using the real clock here would make an otherwise pure recomputation
         * depend on when it ran.
         */
        val STATIC_STAMP = LwwStamp(writtenAt = Instant.EPOCH, by = DeviceId(UNKNOWN_DEVICE))
    }
}
