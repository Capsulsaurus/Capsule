---
title: Notifications
description: What a key-free server can and cannot tell you — local alerts, the contentless wake tier, and the self-hostability rule that decides the transport matrix
status: draft
---

This doc owns alert delivery and the vocabulary. The server holds only ciphertext, and the short-poll sync feed stays the fallback for any push ([Network Resilience](/design/networking/#adverse-network-posture)), so a server can tell a client *that* something changed, never *what*.

## Vocabulary

| Term | Direction | Carries | Owner |
| --- | --- | --- | --- |
| **Alert** | device → its own user | Localized text composed **on-device** | This doc |
| **Wake** | server → client | Nothing: "something changed for you" | This doc |
| **Hint** | server ↔ server, peer ↔ peer | "A new event exists in album A"; advisory, no authority | [Federation](/design/federation/#pull-only-federation), [Peering](/design/peering/#sync) |

Alert classes and trigger predicates live in `capsule-core::notify`, one decision function for every platform; delivery is native per client. Open: the Apple mock lane arms into an in-memory scheduler rather than `UNUserNotificationCenter` until the Rust-backed adapter is composed, the Android app has no flow yet that records the facts the predicate reads, and the pre-armed delivery smoke has not run on a device.

## Tier 0 — Local Alerts

**This is all of v1.**

> **No alert text ever originates from a server.** Clients render copy from their own catalogs, keyed by a class decided locally.

### Alert Classes

A closed enum. Thresholds belong to the trigger owner. Parameters are plain values interpolated into catalog strings, never text.

| Class | Trigger owner | Parameters | Pre-armable |
| --- | --- | --- | --- |
| `sync_stale` | [Sync](/design/sync/#notifications) | `count`, `days_behind` | **Yes** |
| `recovery_check_due` | [Backup — Schedule](/design/backup-recovery/#schedule-and-triggers) | `snooze_budget` (`available`/`spent`), `recovery` (`check`/`rewrap`) | **Yes** |
| `quota_soft` / `quota_grace_expiring` | [Quota](/design/quota/#thresholds-and-states) | `grace` (`counting`/`expired`), second only | No |
| `quarantine_pending` | [Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces) | `count` | No |
| `drop_pending` | [Web Upload](/design/web-upload/#drop-and-adoption-lifecycle) | `count` | No |

### The Pre-Arm Rule

> An alert whose trigger is a **deadline the device can compute** MUST be pre-armed as a scheduled local notification when the deadline becomes known, and MUST be re-armed or cancelled on every state change that moves it.

`sync_stale` reports that the OS gave the app no background time, so it cannot rely on background time; it is armed for two weeks after each successful sync and fires from the OS timer.

Server-side conditions (`quota_*`, `quarantine_pending`, `drop_pending`) cannot be pre-armed and surface at next launch; [Tier 1](#tier-1--wake) is what would close that gap.

The client loop is one idempotent refresh, run after every state change: read the facts, take the badge set from `evaluate_alerts` and the per-class instants from `pre_arm_deadlines`, then reconcile the OS timers against them. A class present holds exactly one timer at its instant (a moved instant or changed copy cancels and re-arms it); a class absent holds none. `next_alert_deadline` is never the client's input: it is the minimum, and arming only it loses the later class's alarm. The armed copy is fixed at arm time, because the alarm fires with the app not running: `sync_stale` carries the current un-synced count and the days to its deadline, `recovery_check_due` its `recovery` split. A tap on `recovery = rewrap` opens the guided re-wrap, never the routine verification prompt.

### Delivery

| Platform | API | Fires with app not running |
| --- | --- | --- |
| iOS | `UNUserNotificationCenter` (calendar triggers at an absolute UTC instant, one request identifier per class) | Yes |
| Android | `NotificationManager` from an `AlarmManager.setAndAllowWhileIdle` alarm, one `PendingIntent` per class, re-armed after reboot | Yes |
| Desktop | Native OS notifications | While running |
| Web | Service-worker `Notification` | Best-effort |
| CLI | Exit codes and stderr | n/a |

Permission is requested the first time a class has something to say, never at first launch: the first refresh that has a timer to arm asks, once. Android alarms are inexact on purpose: a two-week or ninety-day deadline needs no minute precision, an inexact alarm fires late but never early (so its alert is still true on arrival), and an exact one would need the `SCHEDULE_EXACT_ALARM` special-access grant.

### Shared Alert Mechanics

- **Snooze is bounded, then becomes a badge** that never escalates back to an alert. A snooze lasts a day and defers the class's timer to its end; after three consecutive snoozes the class is badge-only until its condition clears, which restores the budget. The recovery check is snoozed by its own cadence scheduler, which owns that budget.
- **Disabling suppresses the warning, never the behavior** it reports on.
- **No alert ever blocks** any flow.
- **Permission denial is not an error:** every class degrades to its in-app badge; no retry, no nag.
- **Copy is a catalog key** under `notification.*` in `locales/` ([i18n](/design/i18n/)): `notification.<class>.title` / `.body`, with an enumerated parameter selecting the key (`notification.recovery_check_due.rewrap.*`, `notification.quota_grace_expiring.expired.*`) because the catalog compiles only a whole-message plural, and a count as that plural's quantity.

## Tier 1 — Wake

**Post-v1, opt-in, off by default.**

### 1. A Wake Carries No Payload

No album, actor, asset, count or timestamp. The client responds with its ordinary poll (`/v1/sync`, `/v1/quota`, `/v1/drops`) and composes any alert locally.

### 2. Wake Is Never a Correctness Dependency

Every client MUST reach the same state with wake permanently unavailable; only timeliness degrades.

### 3. Self-Hostable Transports Only

> **A wake transport MUST require no credential that the operator cannot generate themselves.**

So no **APNs**, **FCM** (bound to the published app) or project relay.

| Transport | Credential | Platforms |
| --- | --- | --- |
| Web Push ([RFC 8030](https://www.rfc-editor.org/rfc/rfc8030)) + VAPID ([RFC 8292](https://www.rfc-editor.org/rfc/rfc8292)) | Operator-generated keypair | Web, desktop |
| UnifiedPush | A distributor the user chooses | Android, desktop |
| `none` (**default**) | — | All |

**iOS has no wake tier, permanently:** events from other actors surface at next launch.

### 4. Registration and Abuse

- **Opt-in per device, revocable.** The endpoint is opaque bytes on the device row, cleared when the device's session is revoked.
- **Coalesced and rate-capped.** The server coalesces behind a floor interval; a client ignores wakes above a rate and falls back to its schedule.
- **Jittered** so wake timing is not a clean activity side-channel.

## Non-Goals

Server-composed or rich notifications; APNs, FCM or relays; comment and reaction alerts; email; presence.

## Validation

Tiers per [Validation Tiers](/design/principles/#validation-tiers).

- **Trigger predicates (unit):** each class fires exactly at its threshold.
- **Pre-arm lifecycle (unit):** arm, re-arm, cancel; never two timers per class.
- **Snooze, disable, denial (unit):** each degrades to a badge and changes no behavior.
- **Pre-armed delivery (smoke, per platform):** fires with the app terminated. A manual step on a device or simulator, because no automated harness can terminate the app and wait out an OS timer: launch a build that arms with the OS (on Apple, the Rust-backed lane), let it pre-arm `sync_stale`, terminate it, move the device clock past the deadline, and observe the notification; tap it and observe the sync-status destination. Repeat with `recovery = rewrap` armed and observe the guided re-wrap, never the routine check. On Android, reboot between arming and firing as well: the alarm must survive `BOOT_COMPLETED`.
- **Wake contentlessness (unit, post-v1):** no library-derived field; response identical to the scheduled poll.
