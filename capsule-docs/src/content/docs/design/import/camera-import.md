---
title: Import — Camera Import
description: Tethered and wireless in-camera import over PTP/IP
status: draft
---

The adapter is `capsule-library::import::camera`. Cameras expose storage over **PTP/IP** (ISO 15740), spoken through the in-house `ptpip-rs` crate — pure Rust and portable to mobile, unlike LGPL libgphoto2. Vendor extensions stay behind that crate: one is chosen from the camera's `DeviceInfo` and runs once after `OpenSession`, Sony first, and a camera no extension claims speaks standard PTP only.

## The Camera Is a Source Adapter

A camera is an import source adapter on the same contract as the filesystem scanner and the [third-party importers](/design/import/pipeline/#third-party-importers): it feeds [Scan & Extract](/design/import/pipeline/#scan--extract), and planning, execution and [upload](/design/transfer/upload/) are unchanged. It adds no pipeline stage and no server surface.

The adapter first copies every object into a client-local staging root, then hands that root to Scan & Extract exactly as a card reader's files would be, so the scanner's grouping (RAW+JPEG stacks) applies unchanged. Staged objects keep the camera's file names under one directory per camera folder, with a name invalid on any supported platform repaired rather than trusted; objects still arriving live outside the scanned tree.

## Contract

- **Deterministic enumeration** by storage id, then object handle, then capture date, so the planner stays deterministic.
- **Dedup by content, not source.** The planner's content-hash dedup prevents re-import; the adapter keeps no "seen" state.
- **Integrity.** PTP/IP carries no checksum and TCP is transport-grade only. Every object is hashed on receipt, its staged bytes are read back and must re-hash to that digest, and a byte count other than the camera's declared size fails the object; the staged file that matched is the one the planner hashes and the [manifest](/design/cryptography/provenance/#asset-manifest) commits to. Corruption fails at import, before signing.
- **Resumable per object**, with partial-object transfer where the camera supports it. A partial file is continued only for the same object (handle, folder, name, size and capture date), since PTP promises handles only within a session, and only from a prefix that was synced to disk and still hashes to the digest recorded when it was. An object already staged is never fetched again; "already staged" means the identity recorded beside the staged file (folder, name, size and capture date) is this object's, so a different object reusing a staged path (a reformatted card restarts its file counter) is received beside it and never overwrites it.
- **Nothing dropped silently.** An object the camera refuses, or whose record cannot be read, and a storage the camera will not list (an empty or unavailable card slot), are reported and the run continues; a lost connection or a failing local disk ends the run and keeps what was staged for the next one.
- **Read-only.** The adapter never mutates camera storage. Any future delete-after-import MUST pass [Verify Before Destroy](/design/transfer/durability/#verify-before-destroy).
- **Discovery** via mDNS (`_ptp._tcp`) or manual IP; pairing state is client-local config.
