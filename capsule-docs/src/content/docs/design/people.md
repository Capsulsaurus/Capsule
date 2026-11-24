---
title: People
description: Face clustering on device, the signed corrections that make every device show the same people, and sharing a person with an album
audit: none
---

People are computed on the client from face embeddings ([](/design/ai/#dense-tagging)) and the user's corrections. The server sees neither: embeddings travel as sealed derivatives ([](/design/ai/#synced-embeddings)) and corrections as sealed operations. This page owns the clustering contract, the people section of the library-settings document and how a person is shared with an album. The models are owned by [](/design/ai/#models-and-algorithms).

**Status.** Planned. Nothing clusters faces yet, and people are named only by labelling a face region ([](/design/organization/#labels)). Orchestration belongs in `capsule-core::ml`, which exists; the people state lives in the library-settings document ([](/design/metadata/#the-library-settings-document)).

## Model

- A **face** is one detection on one asset: `(asset_id, model_id, model_version, box)`, with its embedding. It is AI output and lives only in AI-namespaced derivatives ([](/design/ai/#ai-output-containment)).
- A **person** is a UUIDv7 `person_id` the user has named, merged into or otherwise touched. An untouched cluster is a computed view with no stored identity.
- A **correction** is a signed operation on the people section: it is the only stored people state, and it is user data, never AI output.

People = clustering(faces, corrections). The function is pure: the same faces and corrections give the same people on every device, in sorted `asset_id` order with fixed seeds ([](/design/metadata/#grouping-convergence-requirement)).

## Clustering

Incremental, so a new import never re-clusters the library:

1. **Detect.** A detection whose box overlaps an existing face on the same asset at IoU > 0.5 is that face, not a new one, so re-running detection never duplicates a person.
2. **Core faces.** A face with at least `min_faces` neighbours within cosine distance `max_distance` is a core face. Core faces that are neighbours form one cluster.
3. **Deferred faces.** A face that is not core is deferred and, once the core pass ends, joins the cluster of its nearest core face if that face is within `max_distance`. Otherwise it stays unassigned. Low-quality faces (small boxes, low detector score) use a stricter distance and are never core.
4. **Corrections override.** The correction rules below are applied after clustering and always win.

`min_faces` and `max_distance` are per-model task constants carried by the model's weight-registry release record ([](/design/ai/#weight-registry)), never user settings, so two devices cannot cluster the same faces differently.

## Corrections

The people section of the library-settings document ([](/design/metadata/#the-library-settings-document)) holds every correction. Each map follows the document's LWW-over-`Option` rule, so a stamped `None` is a tombstone and replay is a no-op.

```rust
PeopleSection {
  persons:     Map<person_id,  Lww<Option<Person>>>,
  assignments: Map<FaceAnchor, Lww<Option<Assignment>>>,
}

Person {
  name:        Option<String>,      // ≤ 256 bytes
  merged_into: Option<person_id>,   // set ⇒ this person is an alias of the target
  hidden:      bool,                // ignored: never shown, never suggested
  cover:       Option<FaceAnchor>,
  birth_date:  Option<Date>,        // feeds Memories
}

FaceAnchor = { asset_id: UUIDv7, box: Rect }   // [0, 1], display orientation
Assignment = Is(person_id) | IsNot(person_id) | NotAFace
```

- **Name** creates a `Person` for a cluster and assigns its core faces to it by writing one `Is` per face.
- **Merge** sets `merged_into` on the source. It never deletes the source, so a merge is undone by clearing the field. Chains are followed to their end, and a cycle resolves to the smallest `person_id` in it.
- **Split** and **"not this person"** write `IsNot` for the moved faces, and `Is` for their new person if one is named. A face carrying `IsNot(p)` is never reassigned to `p` by clustering.
- **Not a face** removes a detection from every cluster.
- **Ignore** sets `hidden`. Its faces stay assigned, so the person never reappears as a new cluster.

### Corrections Outlive Models

A correction is anchored by `(asset_id, box)`, not by a model's face id. After a model bump the new detections are matched to anchors at IoU > 0.5 and inherit their assignments. A model change never resets names, merges, hidden people or birth dates. An anchor that matches no new detection is kept verbatim and surfaced as unmatched, never discarded.

## Sharing a Person With an Album

A person's name is private to its owner by default: the library-settings document is keyed so no album co-member can read it.

- **Share.** The owner may publish a person into an album they can write: a record holding `person_id`, the name and up to 16 representative face embeddings, sealed under the album key as album metadata. Co-members' clients match their own clusters against it and **suggest** the name. A suggestion is applied only by the co-member, as a correction in their own library.
- **Edit scope.** A shared record is an LWW register any album writer may edit ("everyone"). A co-member who renamed the person in their own library keeps their name ("only me"): their correction wins on their devices.
- **Unshare.** A stamped `None` removes the record. Names co-members already applied stay in their libraries, because they are now the co-members' own data.

## Search and Smart Albums

- A named person is a search vocabulary entry and the target of `@name` and `person:name` ([](/design/search/#what--binds-to)).
- A smart-album `people_cluster` term ([](/design/organization/#smart-album-definition-schema)) names a `person_id`, which survives model bumps. An unnamed cluster cannot be a term.

## Failure Modes

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Two devices merge the same people concurrently | LWW on `merged_into` | The later write wins; the other merge is a no-op or is undone by clearing |
| A merge cycle | Chain walk | Resolves to the smallest `person_id`; surfaced for the user to fix |
| A model bump re-detects faces | IoU anchor match | Assignments carried over; unmatched anchors surfaced |
| A co-member's shared record names the wrong person | Suggestion only | Nothing changes in the receiver's library until they accept |

## Known Gaps

- Nothing in this page is implemented: no clustering, no people section in the library-settings document, no shared person records. Face boxes and embeddings have no derivative role yet ([](/design/ai/#synced-embeddings)).
- The library-settings document itself is deferred post-v1 with the OGK ([](/design/metadata/#the-library-settings-document)); people corrections wait on it.

## Validation

Tiers per [](/design/principles/#validation-tiers).

- **Clustering determinism (unit).** The same faces and corrections in any arrival order give byte-identical people.
- **Incremental equivalence (unit).** Clustering a library in batches gives the same clusters as clustering it at once, within the deferred-assignment rule.
- **Re-detection (unit).** A second detection at IoU > 0.5 does not create a face.
- **Corrections (unit).** `IsNot` is never overridden by clustering; merge then clear restores both people; cycles resolve to the smallest id.
- **Model bump (unit).** Names, merges and hidden people survive a bump; an unmatched anchor is kept and reported.
- **Shared person (smoke).** A co-member sees a suggestion and no change until accepting; an "only me" rename survives an "everyone" edit.
