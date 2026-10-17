---
title: Smart Albums
description: Albums defined by a query rather than a membership list
status: draft
---

A **smart album** is defined by a predicate rather than a membership list: it names a
query, and its contents are whatever currently matches. An asset that starts matching
appears in it; an edit that takes an asset out of the predicate removes it. Nothing is
copied and no membership row is written, which is why the same result is reachable by
running the query directly.

Because the server holds only ciphertext, every predicate is evaluated **on your own
device**, against the local index. A smart album is a saved query over a library only
your devices can read, not a view the server materializes.

## What a predicate can say

The grammar, its closed field and operator sets, and how two devices converge on a
concurrently-edited definition are owned by
[Organization — System / Smart Albums Views](/design/organization/#system--smart-albums-views).
Definitions travel in the library-settings document rather than in a per-asset sidecar,
because they belong to the library rather than to any one asset.

## The kinds that depend on machine learning

Several album kinds people expect — one per person, trips clustered by place and time,
pets, food, scenes, a best-of-year selection — are predicates over tags and embeddings
that an on-device model produces. Those models, what each is allowed to assert, and how
an embedding records the model version that made it are owned by
[AI/ML Integrations](/design/ai/).

That doc also carries the constraint that shapes them: every device must be able to run
the same model, or a library's organization depends on which phone happened to import a
photo. Model choice is bounded by the weakest supported device, not the strongest.
