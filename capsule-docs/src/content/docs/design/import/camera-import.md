---
title: Import — Camera Import
description: Tethered and wireless in-camera import over PTP/IP
status: draft
---

The adapter is `capsule-library::import::camera`. Cameras expose storage over **PTP/IP** (ISO 15740), spoken through the in-house `ptpip-rs` crate, which is pure Rust and portable to mobile, unlike LGPL [libgphoto2](https://github.com/gphoto/libgphoto2). Vendor extensions stay behind that crate: one is chosen from the camera's `DeviceInfo` and runs once after `OpenSession`, Sony first. A camera no extension claims speaks standard PTP only.

## The Camera Is a Source Adapter

A camera is an import source adapter on the same contract as the filesystem scanner and the third-party importers ([](/design/import/pipeline/#third-party-importers)). It feeds [](/design/import/pipeline/#scan--extract), leaves planning, execution and upload ([](/design/transfer/upload/)) unchanged, and adds no pipeline stage and no server surface.

The adapter first copies every object into a client-local staging root, then hands that root to Scan & Extract exactly as a card reader's files would be, so the scanner's grouping (RAW+JPEG stacks) applies unchanged. Staged objects keep the camera's file names under one directory per camera folder, and a name invalid on any supported platform is repaired rather than trusted. Objects still arriving live outside the scanned tree.

## Contract

- **Deterministic enumeration** by storage id, then object handle, then capture date, so the planner stays deterministic.
- **Dedup by content, not source.** The planner's content-hash dedup prevents re-import; the adapter keeps no "seen" state.
- **Integrity.** PTP/IP carries no checksum and TCP is transport-grade only. Every object is hashed on receipt, its staged bytes are read back and must re-hash to that digest, and a byte count other than the camera's declared size fails the object. The staged file that matched is the one the planner hashes and the manifest ([](/design/cryptography/provenance/#asset-manifest)) commits to, so corruption fails at import, before signing.
- **Resumable per object**, with partial-object transfer where the camera supports it. A partial file is continued only for the same object (handle, folder, name, size and capture date), since PTP promises handles only within a session, and only from a prefix that was synced to disk and still hashes to the digest recorded then.
- **Never fetched twice.** An object already staged is not fetched again. "Already staged" means the identity recorded beside the staged file (folder, name, size and capture date) is this object's, so a different object reusing a staged path (a reformatted card restarts its file counter) is received beside it and never overwrites it.
- **Nothing dropped silently.** An object the camera refuses or whose record cannot be read, and a storage the camera will not list (an empty or unavailable card slot), are reported and the run continues. A lost connection or a failing local disk ends the run and keeps what was staged for the next one.
- **Read-only.** The adapter never mutates camera storage. Any future delete-after-import MUST pass the verify-before-destroy gate ([](/design/transfer/durability/#verify-before-destroy)).
- **Discovery** via mDNS (`_ptp._tcp`) or manual IP; pairing state is client-local config.

## Pairing and the Session

- **Initiator identity.** A client presents one initiator GUID and name to every camera, created on first use and persisted in client-local config, never in a library. A paired camera remembers the GUID, so it is stable across runs and renaming keeps it. The GUID is random (UUIDv4), since every camera the client meets sees it. An unreadable identity file is an error, never silently replaced, because a fresh GUID unpairs every camera.
- **Event channel.** The session's event channel is serviced for as long as the session is open, on its own reader beside the command channel. Every keep-alive probe is answered, so a camera does not drop a client busy in a long transfer, and every event is logged. A failing event channel ends only its reader, never the import.
- **Staging location.** The staging root lives under the library, one per camera (named from its manufacturer, model and serial number), so an interrupted run resumes only that camera's partial objects. Staged objects are kept after the import, which lets the next run skip every object it already holds.

## Discovery

A camera that offers PTP/IP on the network advertises it through DNS-SD as `_ptp._tcp` (port 15740 when it follows the standard). A client browses that type for a **bounded window** and offers what answered. It never waits on the network open-ended, and an empty window is an answer, not a failure.

- **What is offered.** Each answering service instance once, by its instance name, with its host, one address and its port. An IPv4 address is preferred, then a routable IPv6 one. An instance withdrawn inside the window, or with only a link-local IPv6 address or none at all, is not offered. A camera is offered at a plain address, the one a person passes back as the import's address, which carries no interface scope. A link-local IPv6 address without its scope names no interface to connect through, so it is not offered even though the answer arrived on a known interface.
- **Deterministic order.** By instance name, then address, so the same network answers the same list.
- **Choosing implicitly.** An import given no address takes the one camera the window found. None, or several, refuses the run and names what answered, so the person picks one by address. Discovery never guesses between two cameras.
- **Seam.** Browsing sits behind a narrow seam the tests script, so the window, the withdrawal and the choice are proved without the real network.

## The CLI Flow

The CLI is the reference client:

- `capsule camera discover` browses `_ptp._tcp` for a window (`--timeout SECONDS`, 3 by default, at most 60) and lists each camera's name, host, address and port. `--json` prints them as one document.
- `capsule camera import --address HOST[:PORT] --library PATH` connects (port 15740 by default); without `--address` it takes the one camera discovery finds, as above. It receives into the library's staging root, reports every object and storage the camera would not hand over, then plans and executes the staged tree through the same import path `capsule import` runs. `--json` prints one document holding the camera, the discovery answer it was chosen from (`null` when an address was given), the identity presented, the receive tally with each failure, the import report and the event-channel tally.
- `capsule camera identity` shows the identity, creating it on first use, and `--name` renames it.
