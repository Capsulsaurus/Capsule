package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId

/** The trinary culling flag (*Organization - Culling*). [NEUTRAL] is the never-flagged state. */
enum class CullFlag {
    PICK,
    NEUTRAL,
    REJECT,
}

/** A stack's derived cull state, by the precedence `all-rejected -> any-pick -> else mixed`. */
enum class StackCullState {
    ALL_REJECTED,
    ANY_PICK,
    MIXED,
    ;

    companion object {
        /** The state of a stack whose members carry [flags]. No member at all is [MIXED]. */
        fun of(flags: List<CullFlag>): StackCullState =
            when {
                flags.isNotEmpty() && flags.all { it == CullFlag.REJECT } -> ALL_REJECTED
                flags.any { it == CullFlag.PICK } -> ANY_PICK
                else -> MIXED
            }
    }
}

/**
 * Where an asset sits in its stack: the cover first, then by [memberIndex] (`null` sorts
 * last), then in the order the members arrived.
 */
data class StackMembership(
    val stack: StackId,
    val isCover: Boolean = false,
    val memberIndex: Int? = null,
)

/** One asset of the pass, with the flag it carries and the stack it belongs to, if any. */
data class CullAsset(
    val id: AssetId,
    val flag: CullFlag = CullFlag.NEUTRAL,
    val stack: StackMembership? = null,
)

/**
 * Everything a cull pass can be asked to do, named once so the keyboard, the on-screen
 * buttons and the TalkBack custom actions all reach the same verb.
 */
enum class CullVerb(
    /** The flag a verdict writes, or `null` for a verb that is not a verdict. */
    val flag: CullFlag?,
) {
    PICK(CullFlag.PICK),
    NEUTRAL(CullFlag.NEUTRAL),
    REJECT(CullFlag.REJECT),
    NEXT(null),
    PREVIOUS(null),
    EXPAND_STACK(null),
    COLLAPSE_STACK(null),
    SWEEP(null),
}

/**
 * One unit of the pass: a lone asset, or a stack shown by its cover. A stack has no flag of
 * its own: its state is derived from the members every time it is read.
 */
data class CullGroup(
    /** The stack the members share, or `null` for a lone asset. */
    val stack: StackId?,
    /** Cover first, then by `member_index`, then in the order they arrived. */
    val members: List<CullAsset>,
    /** Whether the stack's members are culled one by one rather than through the cover. */
    val isExpanded: Boolean = false,
) {
    /** A stack with more than one member: the only kind there is anything to expand. */
    val isStack: Boolean get() = stack != null && members.size > 1

    /** What the collapsed group shows. */
    val cover: CullAsset get() = members[0]

    /** The derived state. */
    val cullState: StackCullState get() = StackCullState.of(members.map { it.flag })
}

/** One position in the pass: a whole group, or one member of an expanded stack. */
data class CullStep(
    val group: Int,
    /** `null` when the step is the whole group: a lone asset, or a collapsed stack. */
    val member: Int?,
)

/** The flag state a step shows: an asset's own flag, or a collapsed stack's state and size. */
sealed interface CullStepFlag {
    data class Asset(
        val flag: CullFlag,
    ) : CullStepFlag

    data class Stack(
        val state: StackCullState,
        val memberCount: Int,
    ) : CullStepFlag
}

/** The 1-based position of the current step, and the number of steps; `0 of 0` when empty. */
data class CullPosition(
    val index: Int,
    val count: Int,
)

/**
 * One verdict the user gave, as the surface must write it.
 *
 * The pass applies a verdict to what it shows at once; the write follows, and its outcome
 * comes back through [CullingPass.settle] or [CullingPass.revert] carrying this value, so an
 * outcome that arrives after a later verdict on the same asset cannot undo that later one.
 */
data class CullVerdict(
    /** The flag written. */
    val flag: CullFlag,
    /**
     * The stack whose every member the write must reach (the whole stack, not only the
     * members loaded into the pass), or `null` for one asset.
     */
    val stack: StackId?,
    /** The assets in the pass the verdict flagged. */
    val targets: List<AssetId>,
    /** Orders verdicts within one pass. */
    val serial: Int,
) {
    /**
     * The assets the write reaches, given the stack's members as the store lists them:
     * every one of those, then any target the store did not list. Ignores [stackMembers]
     * for a one-asset verdict.
     */
    fun writeTargets(stackMembers: List<AssetId>): List<AssetId> {
        if (stack == null) return targets
        val listed = stackMembers.toSet()
        return stackMembers + targets.filterNot { it in listed }
    }
}

/**
 * The cull pass over one sequence of assets: where the user is, which stacks are open, and
 * the flags as the user last set them. The same model as the Apple client's `CullingPass`.
 *
 * Pure on purpose. Writing a flag, moving to the trash and announcing the result are the
 * surface's business; this decides *what* each verb does to the pass and hands back what a
 * write must reach, so every rule below is a unit test rather than a tap.
 *
 * - A verdict on a collapsed stack flags every member, and any verdict advances to the next
 *   step (the last step stays put). The write reaches the whole stack through
 *   [CullVerdict.stack]: a pass may hold only some members.
 * - A failed write puts back the flag last known stored, on the assets no later verdict has
 *   touched since; a later verdict's own outcome governs those.
 * - Expanding a stack replaces its one step with a step per member, starting on the cover;
 *   collapsing folds them back and lands on the stack.
 * - The sweep candidates are the rejects in the pass, and nothing outside it.
 *
 * Not thread-safe: confine it to one thread, as [CullingModel] does.
 */
class CullingPass(
    assets: List<CullAsset> = emptyList(),
) {
    private val groupList = mutableListOf<CullGroup>()

    /** The groups in pass order. */
    val groups: List<CullGroup> get() = groupList

    /** Every step of the pass, in order. */
    var steps: List<CullStep> = emptyList()
        private set

    /** The current step: a valid index into [steps] unless the pass is empty. */
    var index: Int = 0
        private set

    /** Where each stack's group sits, so a later member joins it in constant time. */
    private val stackGroups = mutableMapOf<StackId, Int>()

    /** The serial the next verdict takes. */
    private var nextSerial = 1

    /** The newest verdict each asset was given, written or not: what the pass shows. */
    private val shown = mutableMapOf<AssetId, Mark>()

    /** The flag each asset is known to hold in the library: what a failed write puts back. */
    private val stored = mutableMapOf<AssetId, Mark>()

    /** The newest verdict given to each collapsed stack, so a later-arriving member shows it. */
    private val stackShown = mutableMapOf<StackId, Mark>()

    /** A flag and the verdict that set it; serial 0 is the flag an asset arrived with. */
    private data class Mark(
        val serial: Int,
        val flag: CullFlag,
    )

    init {
        append(assets)
    }

    // Reading

    val isEmpty: Boolean get() = steps.isEmpty()

    /** The current step, or `null` for an empty pass. */
    val current: CullStep? get() = steps.getOrNull(index)

    /** The asset the current step shows. */
    val currentAsset: CullAsset? get() = current?.let(::assetAt)

    /** The current step's flag state. */
    val currentFlag: CullStepFlag?
        get() {
            val step = current ?: return null
            val group = groupList[step.group]
            if (step.member == null && group.isStack) {
                return CullStepFlag.Stack(group.cullState, group.members.size)
            }
            return CullStepFlag.Asset(assetAt(step).flag)
        }

    val position: CullPosition get() = CullPosition(if (isEmpty) 0 else index + 1, steps.size)

    /** Every rejected asset in the pass, in pass order. */
    val sweepCandidates: List<AssetId>
        get() = groupList.flatMap { group -> group.members.filter { it.flag == CullFlag.REJECT }.map { it.id } }

    /** Whether [verb] would do anything from here: what a button and a key ask first. */
    fun accepts(verb: CullVerb): Boolean {
        val step = current ?: return false
        return when (verb) {
            CullVerb.PICK, CullVerb.NEUTRAL, CullVerb.REJECT -> true
            CullVerb.NEXT -> index < steps.size - 1
            CullVerb.PREVIOUS -> index > 0
            CullVerb.EXPAND_STACK -> step.member == null && groupList[step.group].isStack
            CullVerb.COLLAPSE_STACK -> step.member != null
            CullVerb.SWEEP -> groupList.any { group -> group.members.any { it.flag == CullFlag.REJECT } }
        }
    }

    /** The flags the pass shows for those of [ids] it holds. */
    fun flags(ids: Collection<AssetId>): Map<AssetId, CullFlag> {
        val wanted = ids.toSet()
        return groupList
            .asSequence()
            .flatMap { it.members }
            .filter { it.id in wanted }
            .associate { it.id to it.flag }
    }

    /** The asset [step] shows: the member, or the group's cover. */
    fun assetAt(step: CullStep): CullAsset {
        val group = groupList[step.group]
        return step.member?.let { group.members[it] } ?: group.cover
    }

    // Building

    /**
     * Add assets in sequence order. A member of a stack already in the pass joins it
     * wherever it arrives, so a stack split across two pages is still one step. An asset the
     * user has already given a verdict shows that verdict, whatever flag it was read with.
     */
    fun append(assets: List<CullAsset>) {
        val anchor = currentAsset?.id
        for (arrived in assets) {
            adoptStackVerdict(arrived)
            val asset = shown[arrived.id]?.let { arrived.copy(flag = it.flag) } ?: arrived
            val stack = asset.stack?.stack
            if (stack == null) {
                groupList.add(CullGroup(stack = null, members = listOf(asset)))
                continue
            }
            val at = stackGroups[stack]
            if (at != null) {
                groupList[at] = groupList[at].copy(members = inserted(asset, groupList[at].members))
            } else {
                stackGroups[stack] = groupList.size
                groupList.add(CullGroup(stack = stack, members = listOf(asset)))
            }
        }
        rebuild(anchor)
    }

    /** Drop assets that left the pass (swept to the trash). A stack left empty leaves too. */
    fun remove(ids: Set<AssetId>) {
        if (ids.isEmpty()) return
        val anchor = currentAsset?.id
        val kept =
            groupList
                .map { group -> group.copy(members = group.members.filterNot { it.id in ids }) }
                .filter { it.members.isNotEmpty() }
        groupList.clear()
        groupList.addAll(kept)
        stackGroups.clear()
        groupList.forEachIndexed { at, group -> group.stack?.let { stackGroups[it] = at } }
        rebuild(anchor)
    }

    // Verbs

    /**
     * Apply a verdict to the current step and advance.
     *
     * @return what the surface must write: the whole stack for a collapsed stack, else the
     *   one asset. `null` for an empty pass.
     */
    fun verdict(flag: CullFlag): CullVerdict? {
        val step = current ?: return null
        val group = groupList[step.group]
        val targets = step.member?.let { listOf(group.members[it].id) } ?: group.members.map { it.id }
        val verdict =
            CullVerdict(
                flag = flag,
                stack = if (step.member == null) group.stack else null,
                targets = targets,
                serial = nextSerial,
            )
        nextSerial += 1
        for (member in group.members) {
            if (member.id in targets && member.id !in stored) stored[member.id] = Mark(0, member.flag)
        }
        val mark = Mark(verdict.serial, flag)
        targets.forEach { shown[it] = mark }
        verdict.stack?.let { stackShown[it] = mark }
        setShownFlags(targets.toSet(), flag)
        next()
        return verdict
    }

    /**
     * Record that [verdict]'s write reached [written]: for a stack, every member the library
     * holds, loaded into the pass or not. A member in the pass that no later verdict has
     * touched shows the flag.
     */
    fun settle(
        verdict: CullVerdict,
        written: List<AssetId>,
    ) {
        val mark = Mark(verdict.serial, verdict.flag)
        val reached = mutableSetOf<AssetId>()
        for (id in written.toSet() + verdict.targets) {
            if ((stored[id]?.serial ?: -1) < verdict.serial) stored[id] = mark
            if ((shown[id]?.serial ?: -1) <= verdict.serial) {
                shown[id] = mark
                reached.add(id)
            }
        }
        setShownFlags(reached, verdict.flag)
    }

    /**
     * Undo [verdict] after its write failed: each asset showing it, and no later verdict,
     * goes back to the flag last known stored. Position is left alone: the user is told, and
     * is already looking at the next asset.
     */
    fun revert(verdict: CullVerdict) {
        verdict.stack?.let { stack -> if (stackShown[stack]?.serial == verdict.serial) stackShown.remove(stack) }
        val showing = shown.filterValues { it.serial == verdict.serial }.keys
        for (id in showing) {
            val previous = stored[id] ?: continue
            shown[id] = previous
            setShownFlags(setOf(id), previous.flag)
        }
    }

    /** Move to the next step. `false` at the end of the pass. */
    fun next(): Boolean {
        if (index >= steps.size - 1) return false
        index += 1
        return true
    }

    /** Move to the previous step. `false` at the start of the pass. */
    fun previous(): Boolean {
        if (index <= 0) return false
        index -= 1
        return true
    }

    /** Open the collapsed stack under review, landing on its cover. */
    fun expandStack(): Boolean {
        val step = current
        if (step == null || !accepts(CullVerb.EXPAND_STACK)) return false
        groupList[step.group] = groupList[step.group].copy(isExpanded = true)
        rebuild(null)
        index = steps.indexOf(CullStep(step.group, 0)).takeIf { it >= 0 } ?: index
        return true
    }

    /** Fold the expanded stack the current member belongs to, landing on it. */
    fun collapseStack(): Boolean {
        val step = current
        if (step == null || !accepts(CullVerb.COLLAPSE_STACK)) return false
        groupList[step.group] = groupList[step.group].copy(isExpanded = false)
        rebuild(null)
        index = steps.indexOf(CullStep(step.group, null)).takeIf { it >= 0 } ?: index
        return true
    }

    // Internals

    /**
     * Give an arriving stack member its stack's verdict when that verdict is newer than any
     * the member was given itself, remembering the flag it arrived with.
     */
    private fun adoptStackVerdict(asset: CullAsset) {
        val stack = asset.stack?.stack ?: return
        val mark = stackShown[stack] ?: return
        if ((shown[asset.id]?.serial ?: 0) >= mark.serial) return
        if (asset.id !in stored) stored[asset.id] = Mark(0, asset.flag)
        shown[asset.id] = mark
    }

    /** Show [flag] on every member of the pass whose id is in [ids]. */
    private fun setShownFlags(
        ids: Set<AssetId>,
        flag: CullFlag,
    ) {
        if (ids.isEmpty()) return
        for (at in groupList.indices) {
            val group = groupList[at]
            if (group.members.none { it.id in ids }) continue
            groupList[at] = group.copy(members = group.members.map { if (it.id in ids) it.copy(flag = flag) else it })
        }
    }

    /**
     * Recompute the steps, keeping the user on the asset they were looking at when it is
     * still in the pass, and otherwise on the step that took its place.
     */
    private fun rebuild(anchor: AssetId?) {
        steps =
            groupList.indices.flatMap { at ->
                val group = groupList[at]
                if (group.isStack && group.isExpanded) {
                    group.members.indices.map { CullStep(at, it) }
                } else {
                    listOf(CullStep(at, null))
                }
            }
        val found = anchor?.let { id -> steps.indexOfFirst { assetAt(it).id == id } } ?: -1
        index = if (found >= 0) found else minOf(index, maxOf(steps.size - 1, 0))
    }

    private companion object {
        /** [members] with [asset] inserted in cover-first, `member_index` order, after equals. */
        fun inserted(
            asset: CullAsset,
            members: List<CullAsset>,
        ): List<CullAsset> {
            val at = members.indexOfFirst { MEMBER_ORDER.compare(it, asset) > 0 }.takeIf { it >= 0 } ?: members.size
            return members.toMutableList().apply { add(at, asset) }
        }

        val MEMBER_ORDER: Comparator<CullAsset> =
            compareBy<CullAsset> { if (it.stack?.isCover == true) 0 else 1 }
                .thenBy { it.stack?.memberIndex ?: Int.MAX_VALUE }
    }
}
