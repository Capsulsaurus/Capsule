---
title: Your Whole Library, Offline
description: A full index of every photo you have ever had, on every device, with or without a connection
status: draft
---

Most photo services give a device a window onto a library that lives on someone else's
server. Without a connection, the window closes. Capsule is designed the other way round: every
device holds a **full index of every photo and video you have ever kept**, so the whole library
can be browsed, searched and organized offline. Capsule has not been released yet, so this page
describes the design.

## What you get

- **Every asset, always listed.** The timeline, albums, labels and search cover the whole
  library on every device. Being offline never removes an asset, album or index entry; it only
  delays fetching bytes the device does not hold yet.
- **Something to see at once.** Each asset's placeholder arrives with its metadata, and
  thumbnails and previews fill in as space and connection allow. Originals are fetched only when
  you ask for them.
- **Edits that wait for you.** Picks, ratings, labels and captions edited offline are signed on
  the device and sync when a connection returns.

## Why this is possible

The server holds only ciphertext, so search and organization already run on your devices over a
local index rather than on the server. Keeping that index complete on every device is what makes
the offline library the default rather than an extra. The contract is
[Local Gallery](/design/local-gallery/); what each device fetches and keeps is
[Sync](/design/sync/) and [Download](/design/transfer/download/).
