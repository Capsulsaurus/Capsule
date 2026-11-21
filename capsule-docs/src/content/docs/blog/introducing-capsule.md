---
title: Introducing Capsule
description: How a photo web app became an end-to-end encrypted library that promises never to lose your data.
date: 2026-11-09
authors: justin
tags:
  - announcement
status: draft
# Unpublished: Starlight excludes `draft: true` pages from production builds.
# The outline below is a placeholder until the post is written.
draft: true
---

Capsule started in August 2024 as Pixles, a web app with an API in front of it. Two years later it
is a Rust-first, end-to-end encrypted photo library whose most important promise is a negative one:
a version mismatch may refuse a write, but it never loses your data.

<!-- excerpt -->

## Outline

- **From Pixles to Capsule.** The first commits were a TanStack web app and an API. Three months in,
  the API was replaced with a Rust base, and the Swift clients followed in 2025. _TODO: when and why
  the project was renamed._
- **Principles with teeth.** Sidecars are canonical and the database is a cache that can be rebuilt.
  A feature works only online or only offline, never differently depending on connectivity. Postel's
  law applies within a schema version and not across versions. ([](/design/principles/))
- **Nine invariants.** These include: the server never rewrites client-signed data, a client never
  discards data to resolve a mismatch, and nothing writes what it doesn't understand.
  ([](/design/versioning/#system-invariants))
- **How it's built.** The CLI is the reference client and every flow is a `--json` subcommand. Most
  of the work is agent-driven, under invariants that the checks cite. _TODO: first-person story._
- **Where it is and how to follow along.** The mobile clients are being stabilized before the beta.
  ([Roadmap](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md))
