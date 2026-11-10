---
title: Sharing across servers
description: Why Capsule shipped federation switched off, and what the design that replaced it refuses to trust.
date: 2026-11-10
authors: justin
tags:
  - engineering
  - federation
status: draft
# Unpublished: Starlight excludes `draft: true` pages from production builds.
# The outline below is a placeholder until the post is written.
draft: true
---

We shipped federation turned off. Its routes went behind a flag before they were exposed, and they
stayed there until the protocol could survive a peer that lies.

<!-- excerpt -->

## Outline

- **Off by default.** `FEDERATION_ENABLED` was added and left false ([#597](https://github.com/Capsulsaurus/Capsule/pull/597)). Hardening
  followed: no budget is charged until a signature verifies, reports carry nonces and a freshness
  window, and a full rate-limit partition returns 429 instead of 500 ([#912](https://github.com/Capsulsaurus/Capsule/pull/912)).
- **Pull, never push.** A peer may only send a signed hint. The receiver pulls and re-validates on its
  own terms, and each album has one home server, so there is no need for consensus
  ([#1123](https://github.com/Capsulsaurus/Capsule/pull/1123), [](/design/federation/)).
- **The caption that leaked a name.** A rewritten caption survived in CRDT history and reached a
  share-link recipient. Strict export now keeps only each field's current value ([#904](https://github.com/Capsulsaurus/Capsule/pull/904),
  [](/design/share-links/)).
- **Moderation without seeing content.** Moderation does no inspection and no silent actions. Peers
  act as opaque couriers that never hold album keys ([](/design/moderation/), [](/design/peering/)).
- **Lesson.** _TODO._ ([](/design/threat-model/))
