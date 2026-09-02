---
title: Notifications
description: What a key-free server can and cannot tell you — local alerts, the contentless wake tier, and the self-hostability rule that decides the transport matrix
status: draft
---

Capsule has always had notifications; it has never had a delivery contract. The
[two-week staleness prompt](/design/import/download-sync/#notifications) says "the user is notified"
and stops there — no API, no permission model, no answer to *how*. Meanwhile "notification" already
names two unrelated things in these docs: the user-visible warning above, and the low-trust wire
signal a [federated peer](/design/federation/#pull-only-federation) or a
[LAN peer](/design/peering/#transfer-protocol) sends to prompt a pull.

This doc owns the delivery contract and fixes the vocabulary. It is bounded by two facts that are
settled elsewhere and are not relitigated here:

- **The server holds only ciphertext.** It cannot read an album title, an actor's name, or a photo.
  Whatever a server sends, it cannot be a meaningful sentence about the user's library.
- **The sync feed is short-poll by construction**, and
  [Network Resilience](/design/networking/#adverse-network-posture) already requires that any future
  push optimization keep the unary poll as its fallback path.

Together these bound the entire design space: a server can tell a client *that* something changed,
never *what*. Everything below follows from that.

## Vocabulary

Three distinct things, three words. The collision this table resolves is the reason the doc exists.

| Term      | Direction                     | Carries                                                   | Owner |
| --------- | ----------------------------- | --------------------------------------------------------- | ----- |
| **Alert** | device → its own user         | Localized text, composed **on-device** from decrypted state | This doc |
| **Wake**  | server → client               | Nothing. A contentless "something changed for you"          | This doc |
| **Hint**  | server ↔ server, peer ↔ peer  | "A new event exists in album A" — advisory, authority-free  | [Federation](/design/federation/#pull-only-federation), [Peering](/design/peering/#transfer-protocol) |

A hint is a *pull prompt between infrastructure*. A wake is a hint applied at the client edge. An
alert is the only one a human ever sees, and it is never produced by a server.

**Where this lives.** Alert classes and their trigger predicates live in `capsule-core::notify`
so every platform evaluates one shared decision function; only *delivery* is native per client.
This is the [minimal-divergence split](/design/clients/#design-priorities) applied to alerts. No
server module is planned for v1 — Tier 0 has no server half.

**Status.** `capsule-core::notify` is **built** (slice `S-D29`, core half): one pure decision
function returns the classes true at an instant, a second returns the instant to arm per class,
and `capsule-sdk::ffi` carries both to the apps. Every predicate input is caller-supplied,
because the core holds none of the trigger state. What is still owed is the *delivery* half —
the per-platform scheduling and presentation below, the `notification.*` catalog keys, and the
permission request — so no alert on this page reaches a user yet.

## Tier 0 — Local Alerts

**This is the whole of v1.** Every alert is composed on-device from state the device already holds.

> **No alert text ever originates from a server.** A client renders alert copy from its own
> catalogs, keyed by an alert class it decided locally. A server cannot supply, localize, or
> influence the words a user reads.

This is not a privacy nicety layered on top — it is forced. A key-free server has no plaintext to
write a sentence from.

### Alert Classes

A closed enum. Each class's *trigger predicate and thresholds* stay owned by the doc that defines the
condition; this doc owns the class list, the delivery, and the shared snooze/badge mechanics.

| Class | Trigger owner | Pre-armable |
| --- | --- | --- |
| `sync_stale` | [Download & Sync — Notifications](/design/import/download-sync/#notifications) | **Yes** |
| `recovery_check_due` | [Backup — Schedule and Triggers](/design/backup-recovery/#schedule-and-triggers) | **Yes** |
| `quota_soft` / `quota_grace_expiring` | [Quota — Thresholds and States](/design/quota/#thresholds-and-states) | No |
| `quarantine_pending` | [Threat Model — Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces) | No |
| `drop_pending` | [Web Upload — Drop and Adoption Lifecycle](/design/web-upload/#drop-and-adoption-lifecycle) | No |

Unknown classes are rejected as structural errors, like every other closed enum
([Schema Rules](/design/threat-model/schema-rules/)).

### The Pre-Arm Rule

The staleness alert as originally written could not fire. It is the compensating control for
[background execution](/design/import/download-sync/#background-execution) granting nothing — but if
it is evaluated *during* a background window, then the very condition it exists to report (the OS
scheduled nothing for weeks) is the condition that prevents it firing. The alert and its trigger were
the same starved process.

The fix is to stop evaluating at fire time:

> An alert whose trigger is a **deadline the device can compute** MUST be pre-armed as a scheduled
> local notification at the moment that deadline becomes known — not evaluated when it expires. It
> MUST be re-armed or cancelled on every state change that moves the deadline.

`sync_stale` is armed for *now + two weeks* at the end of each successful sync, and re-armed by the
next one; `recovery_check_due` is armed at each cadence step. Both then fire from the OS's own timer,
whether or not the app has run since. Neither needs a background window, which is precisely the
point.

**The honest boundary.** Pre-arming works only for deadlines a device can compute alone. The other
three classes — `quota_*`, `quarantine_pending`, `drop_pending` — depend on state that lives on the
server, so on a device that never runs they cannot fire at all. That gap is real, it is what
[Tier 1](#tier-1--wake) exists for, and v1 accepts it: those three surface at next app launch.

### Delivery

Native platform APIs, no abstraction layer:

| Platform | API | Fires while the app is not running |
| --- | --- | --- |
| iOS | `UNUserNotificationCenter` with `UNCalendarNotificationTrigger` / `UNTimeIntervalNotificationTrigger` | Yes |
| Android | `NotificationManagerCompat`, scheduled via `AlarmManager.setExactAndAllowWhileIdle` (or `WorkManager` where inexact timing is acceptable) | Yes |
| Desktop | The OS's native notification facility, armed by the self-throttling scheduler | While the process runs |
| Web | `Notification` from the registered service worker | Best-effort; treat absence as normal |
| CLI | None — exit codes and stderr | n/a |

Android requires `POST_NOTIFICATIONS` from API 33; iOS requires an authorization prompt. Neither is
requested at first launch: an alert class asks for permission the first time it has something to
say, so the prompt carries context.

### Shared Alert Mechanics

Uniform across every class; the per-class parameters stay with the trigger owners.

- **Snooze is bounded, then becomes a badge.** A class may be snoozed a limited number of consecutive
  times; past that bound the alert stops re-firing and degrades to a persistent, non-blocking badge
  on the relevant surface. The badge never escalates back into an alert on its own.
- **Disabling suppresses the warning, never the behavior.** Turning off `sync_stale` does not turn
  off auto-sync; turning off `recovery_check_due` does not stop the recovery check mattering. An
  alert is a report about a condition, never the mechanism that manages it.
- **No alert ever blocks.** Not sync, not unlock, not upload, not any critical flow. Alerts are
  advisory by construction.
- **A permission denial is not an error.** If the OS refuses notification authorization, every class
  degrades to its in-app badge. Nothing retries, nothing nags, and no flow fails.
- **Alert copy is a catalog key.** Strings live under a reserved `notification.*` namespace in
  `locales/` per the [i18n contract](/design/i18n/). Keys land with the implementing slice, because
  the i18n guard requires a live consumer.

## Tier 1 — Wake

**Post-v1, opt-in, and off by default.** Specified here so the contract is settled before anyone
builds it, not because v1 ships it.

A wake is the client-edge analogue of the federation hint: it prompts a pull sooner than the
schedule would, and carries no authority.

### 1. A Wake Carries No Payload

No album id, no actor, no asset id, no count, no timestamp beyond what the transport itself requires.
The client's only correct response is to run the ordinary `GET /v1/sync` (and `GET /v1/quota`, `GET /v1/drops`)
it would eventually have run anyway, then compose any resulting alert locally from Tier 0.

This is not merely a privacy preference. A server that cannot read the library has nothing truthful
to put in a payload, and a wake that carried server-authored content would be the one place in the
system where a compromised server could write words directly onto a user's lock screen.

### 2. Wake Is Never a Correctness Dependency

Inherited from [Network Resilience](/design/networking/#adverse-network-posture): the unary poll
remains the correctness path. Every client MUST behave identically — same eventual state, same
guarantees — with wake permanently unavailable. A deployment with wake disabled is fully functional;
only timeliness degrades. Wake is a latency optimization and nothing more.

### 3. Self-Hostable Transports Only

This is the rule that decides the platform matrix, and it is stated as a rule rather than a list so
future transports are judged rather than argued about:

> **A wake transport MUST require no credential that the operator cannot generate themselves.**

Capsule's deployment profile requires **zero third-party accounts** today — every secret an operator
holds is one they or the server generated. A transport that breaks that property makes self-hosting
depend on a commercial relationship with a third party, which is a categorical change to what
self-hosting *is*, not an incremental cost.

**Excluded by the rule:**

- **APNs** — the auth key is bound to Capsule's Apple team and app bundle id. A self-hoster cannot
  mint one, because they did not publish the app. Serving them would require a project-operated
  relay that every self-hosted deployment's wake traffic flows through.
- **FCM** — same shape: the sender identity is baked into the shipped APK, so an operator cannot
  substitute their own project either.

**Admitted:**

| Transport | Credential | Platforms |
| --- | --- | --- |
| Web Push ([RFC 8030](https://www.rfc-editor.org/rfc/rfc8030)) with VAPID ([RFC 8292](https://www.rfc-editor.org/rfc/rfc8292)) | An operator-generated keypair | Web, desktop |
| UnifiedPush | A distributor the *user* chooses and can self-host | Android, desktop |
| `none` (**default**) | — | All |

**iOS has no wake tier, permanently.** APNs is the only path to a not-running iOS app, and the rule
above excludes it. The consequence, stated plainly rather than left to be discovered: on iOS, events
originating from another actor — an album share, a guest drop, a quota grace window counting down —
surface at next app launch and not before. That is a deliberate trade of timeliness for the
zero-third-party-credential property, not an unimplemented gap.

### 4. Registration and Abuse

- **Opt-in per device and revocable.** A wake endpoint is stored as opaque bytes on the device row
  and cleared when that device's session is revoked. Enabling it is a per-device user action.
- **A wake channel is an amplifier.** Server-side, wakes to a device are coalesced behind a floor
  interval. Client-side, a device that receives wakes above a rate ignores the excess and falls back
  to its ordinary schedule — a flood degrades to the polling behavior that was already correct.
- **Jitter.** Coalesced wakes are dispatched with jitter, so wake timing does not become a clean
  side-channel for activity timing to anyone observing the push path.

## Non-Goals

Recorded so they are not relitigated:

- **Server-composed alert text**, and **rich notifications** carrying thumbnails, actor names, or
  counts. Both are impossible against a key-free server, not merely unimplemented.
- **APNs, FCM, or any project-operated push relay** — see [the rule](#3-self-hostable-transports-only).
- **Comment and reaction alerts.** No such surface exists in the design.
- **Email or SMTP delivery.** Capsule has no outbound mail channel and is not gaining one here.
- **Presence.** Named alongside notifications in [Federation](/design/federation/#pull-only-federation)
  as low-trust, and out of scope in both tiers.

## Validation

Tiers per [Validation Tiers](/design/principles/#validation-tiers).

- **Trigger predicates (unit).** Each alert class's predicate over fixture state; assert it fires
  exactly at its threshold and not before.
- **Pre-arm lifecycle (unit).** Under a mocked clock — mirroring the
  [recovery-cadence test](/design/backup-recovery/) — assert arm on deadline-known, re-arm on the
  state change that moves it, cancel when the condition clears, and that no path leaves two live
  timers for one class.
- **Snooze bounds (unit).** Consecutive snoozes stop at the bound and degrade to a badge; a disabled
  class never fires; disabling changes no underlying behavior.
- **Permission denial (unit).** A refused authorization degrades every class to its badge, returns no
  error, and blocks no flow.
- **Pre-armed delivery (smoke, per platform).** With the app terminated, an armed alert fires from
  the OS timer.
- **Wake contentlessness (unit, post-v1).** Assert a wake envelope carries no library-derived field,
  and that a client's response to one is byte-identical to its scheduled poll.

**No E2E case is added** — the bounded surface in
[Module Map — E2E Test Surface](/design/module-map/#e2e-test-surface) is unchanged.
