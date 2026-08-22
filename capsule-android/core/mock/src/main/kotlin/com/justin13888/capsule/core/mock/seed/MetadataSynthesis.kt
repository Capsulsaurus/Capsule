package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.Lww
import com.justin13888.capsule.core.model.LwwStamp
import com.justin13888.capsule.core.model.ModelId
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.ModelVersion
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SupersededCaption
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.asWire
import java.time.Duration

/** The model slot the library's AI-derived values were produced by today. */
internal val CURRENT_TAG_SLOT = ModelSlot(ModelId("mobileclip-b"), ModelVersion("2026.04"))

/** A slot that has since been superseded — its outputs are kept, and excluded from queries. */
internal val STALE_TAG_SLOT = ModelSlot(ModelId("mobileclip-b"), ModelVersion("2025.09"))

/** The face-embedding slot in use today. */
internal val CURRENT_FACE_SLOT = ModelSlot(ModelId("facenet-s"), ModelVersion("2026.02"))

/** A superseded face slot, so the regeneration queue has something in it. */
internal val STALE_FACE_SLOT = ModelSlot(ModelId("facenet-s"), ModelVersion("2025.05"))

/**
 * The collaborative metadata layered over the assets: captions, tags, people and saved
 * searches.
 *
 * Runs after every frame exists, because all four of them reference assets by id. Coverage is
 * deliberately sparse — a library where every photo carries a caption and six tags is not one
 * anybody has ever had, and it would hide the empty states the UI has to handle.
 */
internal fun LibraryGenerator.synthesizeCollections() {
    synthesizeCaptions()
    synthesizeTags()
    synthesizePeople()
    synthesizeSmartAlbums()
}

/** Captions on a small share of assets, one of them carrying a displaced concurrent edit. */
private fun LibraryGenerator.synthesizeCaptions() {
    val prng = metadataPrng.derive("captions")
    val subjects = assets.filterIndexed { index, _ -> index % CAPTION_STRIDE == 0 }
    subjects.forEachIndexed { index, asset ->
        val writtenAt = asset.importTimestamp.plus(Duration.ofHours(prng.nextInt(1, CAPTION_DELAY_HOURS).toLong()))
        val text = "${prng.pick(CAPTION_SUBJECTS)} ${prng.pick(CAPTION_QUALIFIERS)}"
        // Every so often a second device captioned the same photo minutes apart. The loser is
        // kept rather than dropped: a plain last-writer-wins register would silently discard
        // one side of a real edit, and the UI has to be able to offer it back.
        val superseded =
            if (index % SUPERSEDED_STRIDE == 0 && devices.size > 1) {
                listOf(
                    SupersededCaption(
                        value = "${prng.pick(CAPTION_SUBJECTS)} ${prng.pick(CAPTION_QUALIFIERS)}",
                        writtenBy = devices[1].id,
                        writtenAt = writtenAt.minus(Duration.ofMinutes(SUPERSEDED_MINUTES_EARLIER)),
                    ),
                )
            } else {
                emptyList()
            }
        captions[asset.id] =
            Caption(
                current = Lww(text, LwwStamp(writtenAt, thisDevice.id)),
                superseded = superseded,
            )
    }
}

/**
 * Both tag namespaces, kept structurally apart.
 *
 * The AI set is far denser than the user set, which is the real ratio: a model tags
 * everything and a person tags almost nothing. Keeping them in separate fields is what makes
 * "can a model overwrite what I typed?" a question that cannot arise.
 */
private fun LibraryGenerator.synthesizeTags() {
    val prng = metadataPrng.derive("tags")
    assets.forEachIndexed { index, asset ->
        val user =
            if (index % USER_TAG_STRIDE == 0) {
                List(prng.nextInt(1, MAX_USER_TAGS)) { UserTag(prng.pick(USER_TAG_WORDS), nextAddId()) }
            } else {
                emptyList()
            }
        val ai =
            if (index % AI_TAG_STRIDE == 0) {
                // Every so often the entry came from a model version that has since been
                // swapped. It is marked stale and kept — the original is still there to
                // regenerate from, and deleting it would lose the fact that something was
                // suggested here at all.
                val stale = index % STALE_AI_TAG_STRIDE == 0
                List(prng.nextInt(1, MAX_AI_TAGS)) {
                    AiTag(
                        tag = prng.pick(AI_TAG_WORDS),
                        addId = nextAddId(),
                        slot = if (stale) STALE_TAG_SLOT else CURRENT_TAG_SLOT,
                        isStale = stale,
                    )
                }
            } else {
                emptyList()
            }
        if (user.isNotEmpty() || ai.isNotEmpty()) {
            tags[asset.id] = AssetTags(user = user, ai = ai)
        }
    }
}

/**
 * Face clusters, and the people the user attached to some of them.
 *
 * A cluster's members are listed in sorted asset-id order because that is what the design
 * requires of the real clustering pass — the computation is a pure function of the asset set
 * and the model slot, and a client that ordered them any other way would produce a different
 * cover photo than its own other devices.
 */
private fun LibraryGenerator.synthesizePeople() {
    val candidates =
        assets
            .filter { it.isInDefaultViews && it.duration == null }
            .let { pool -> pool.filterIndexed { index, _ -> index % FACE_ASSET_STRIDE == 0 } }
    if (candidates.isEmpty()) return
    val perCluster = (candidates.size / CLUSTER_COUNT).coerceAtLeast(1)
    val builtClusters =
        (0 until CLUSTER_COUNT).map { index ->
            val slice =
                candidates
                    .drop(index * perCluster)
                    .take(perCluster)
                    .map(Asset::id)
                    .sortedBy { it.value }
            // The last cluster is left on a superseded slot on purpose: a stale cluster is the
            // regeneration queue, and a user who watches a person vanish after an app update
            // is owed the explanation rather than the disappearance.
            val stale = index == CLUSTER_COUNT - 1
            FaceCluster(
                id = FaceClusterId(DeterministicIds.uuidV7(index.toLong(), idPrng)),
                slot = if (stale) STALE_FACE_SLOT else CURRENT_FACE_SLOT,
                status = (if (stale) ModelOutputStatus.Stale else ModelOutputStatus.Current).asWire(),
                assetIds = slice,
                coverAssetId = slice.firstOrNull(),
            )
        }
    clusters += builtClusters
    PEOPLE_NAMES.forEachIndexed { index, name ->
        val attached = builtClusters.getOrNull(index) ?: return@forEachIndexed
        people +=
            Person(
                id = PersonId(DeterministicIds.uuidV7(index.toLong(), idPrng)),
                displayName = name,
                clusterIds = listOf(attached.id),
                coverAssetId = attached.coverAssetId,
                assetCount = attached.assetIds.size,
                isFavourite = index < FAVOURITE_PEOPLE,
            )
        clusters[index] = attached.copy(personId = people.last().id)
    }
}

/**
 * Saved searches, including one this build must preserve without evaluating.
 *
 * The forward-schema definition is the important one: a client that stripped it would lose
 * work the user did on a newer device, and a client that guessed at its terms would compute a
 * different membership than the device that wrote it.
 */
private fun LibraryGenerator.synthesizeSmartAlbums() {
    val stamp = LwwStamp(spec.now.minus(Duration.ofDays(SMART_ALBUM_AGE_DAYS)), thisDevice.id)
    smartAlbums +=
        SmartAlbumDefinition(
            id = SmartAlbumId(DeterministicIds.uuidV7(1, idPrng)),
            predicateSchema = 1,
            displayName = Lww("Four stars and up", stamp),
            predicate =
                Predicate.Leaf(
                    Term(
                        field = QueryField.Rating.asWire(),
                        op = Operator.Gte.asWire(),
                        operand = Operand.Number(FOUR_STARS),
                    ),
                ),
        )
    smartAlbums +=
        SmartAlbumDefinition(
            id = SmartAlbumId(DeterministicIds.uuidV7(2, idPrng)),
            predicateSchema = 1,
            displayName = Lww("Everything I picked", stamp),
            predicate =
                Predicate.Leaf(
                    Term(
                        field = QueryField.Cull.asWire(),
                        op = Operator.Is.asWire(),
                        operand = Operand.CullLiteral(com.justin13888.capsule.core.model.CullFlag.Pick),
                    ),
                ),
        )
    smartAlbums +=
        SmartAlbumDefinition(
            id = SmartAlbumId(DeterministicIds.uuidV7(3, idPrng)),
            predicateSchema = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA + 1,
            displayName = Lww("Made on a newer device", stamp),
            predicate =
                Predicate.Leaf(
                    Term(
                        field =
                            com.justin13888.capsule.core.model.Wire
                                .Unknown("scene_classification"),
                        op =
                            com.justin13888.capsule.core.model.Wire
                                .Unknown("resembles"),
                        operand = Operand.StringSet(listOf("mountains")),
                    ),
                ),
        )
}

private const val CAPTION_STRIDE = 37
private const val CAPTION_DELAY_HOURS = 96
private const val SUPERSEDED_STRIDE = 11
private const val SUPERSEDED_MINUTES_EARLIER = 14L
private const val USER_TAG_STRIDE = 13
private const val MAX_USER_TAGS = 3
private const val AI_TAG_STRIDE = 4
private const val MAX_AI_TAGS = 4
private const val STALE_AI_TAG_STRIDE = 40
private const val FACE_ASSET_STRIDE = 53
private const val CLUSTER_COUNT = 9
private const val FAVOURITE_PEOPLE = 3
private const val SMART_ALBUM_AGE_DAYS = 210L
private const val FOUR_STARS = 4L

private val CAPTION_SUBJECTS =
    listOf("Morning light", "The long walk back", "Second attempt", "Golden hour", "Rain on the window")

private val CAPTION_QUALIFIERS =
    listOf("— finally sharp", "before the crowd arrived", "handheld at 1/15", "worth the early start", "on film")

private val USER_TAG_WORDS =
    listOf("portfolio", "print", "family", "reshoot", "client", "keeper", "iceland", "kyoto", "studio")

private val AI_TAG_WORDS =
    listOf("mountain", "beach", "food", "dog", "night", "architecture", "snow", "portrait", "street", "water")

private val PEOPLE_NAMES =
    listOf("Mira", "Tomas", "Yuki", "Priya", "Sam", "Elena", "Noor", "Kai")
