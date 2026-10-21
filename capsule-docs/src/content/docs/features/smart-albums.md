---
title: Smart Albums
description: Albums defined by a query rather than a membership list
status: draft
---

A **smart album** is a saved query: its contents are whatever currently matches, so
nothing is copied and no membership is stored. The server holds only ciphertext, so
every predicate is evaluated **on your own device**.

## What a predicate can say

The grammar and merge rules are owned by
[Organization — System / Smart Albums Views](/design/organization/#system--smart-albums-views).

## The kinds that depend on machine learning

Albums per person, trip, pet or scene are predicates over on-device model output; see
[AI/ML Integrations](/design/ai/). Model choice is bounded by the weakest supported
device, so every device organizes a library the same way.
