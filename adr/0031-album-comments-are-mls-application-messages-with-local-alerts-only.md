# ADR-0031 — Album comments and reactions are MLS application messages, with local alerts only

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [MLS § Comments and Reactions](../capsule-docs/src/content/docs/design/cryptography/mls.md#comments-and-reactions), [Notifications § Alert Classes](../capsule-docs/src/content/docs/design/notifications.md#alert-classes), [Authorization § Partner Sharing](../capsule-docs/src/content/docs/design/authorization.md#partner-sharing)
- **Issues:** #1294, #1295

## Context

Shared albums had no conversation: no comments, no likes. The notifications doc listed comment
and reaction alerts as a non-goal, together with server-composed notifications, push relays and
email. The non-goal protected one property: the server never learns or composes the content of
a notification. It did not need to rule out the activity itself.

Both self-hosted photo servers that Capsule's users compare it with have album comments and
likes. The end-to-end encrypted one encrypts them with the album key, and its users asked for
them (and for family-wide library sharing) more than for most features. Shared albums without
them feel inert.

Sharing was also strictly album by album. Couples and families wanted "share everything with my
partner", which the server cannot do on its own under end-to-end encryption, because adding a
member needs the album keys.

## Decision

Comments and reactions are `AlbumActivity` records, sent as MLS application messages of the
current epoch and also kept as records sealed under that epoch's AMK so history follows the
album's `history_policy`. A device raises a local `album_activity` alert only after it has
synced the activity itself. The non-goal is narrowed to alerts a server composes or relays.

Partner sharing is a signed grant in the owner's library settings, carried out by the owner's
own devices: they add the partner to every album the grant covers, at creation or on the next
sync.

## Consequences

- The server stores one more kind of sealed record per album and learns only its size and
  timing, the same as any metadata write.
- Readers can now write to an album, but only activity, never assets or metadata.
- A partner grant is never instant when no owner device is online; the UI states the delay
  rather than claiming the album is shared.
- `album_activity` is a new value of the closed alert-class enum.

## Considered and rejected

- **Server-relayed comment notifications.** They would need a payload or a push relay, both
  still non-goals.
- **Comments from share-link viewers with anonymous identities.** A link viewer is not a group
  member, so their activity could not be authenticated by MLS; it can be revisited separately.
- **Server-side partner sharing.** The server holds no keys, so it cannot add anyone to an
  album's group.
