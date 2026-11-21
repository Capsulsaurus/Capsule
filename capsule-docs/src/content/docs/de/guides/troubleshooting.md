---
title: Fehlerbehebung
description: Sammeln Sie die Version und die Logs, die ein Bericht braucht, und beheben Sie die häufigen Probleme mit einem Capsule-Server, einem Import oder einer Bibliothek.
status: draft
---

Beginnen Sie hier, wenn etwas schiefgeht. Wenn nichts davon hilft, erfahren Sie unter
[](/support/), wo Sie fragen können, und die Diagnosedaten aus dem ersten Abschnitt sind das, was
ein Fehlerbericht braucht.

<a id="collect-diagnostics"></a>

## Diagnosedaten sammeln

### Version

Es wurde noch kein Release veröffentlicht, daher unterscheidet die Versionsnummer keine Builds.
Alles wird aus dem Quellcode gebaut, also melden Sie den Commit, aus dem Sie gebaut haben:
`git rev-parse --short HEAD` in Ihrem Checkout. Die CLI hat kein Versions-Flag, und
`capsule-server --version` gibt nur die Crate-Version aus, die alle Builds gemeinsam haben.

### Logs

CLI und Server schreiben Logs nach **stderr**. Stdout enthält nur, was ein Befehl ausgibt, und
unter `--json` ist das ein einzelnes JSON-Dokument, sodass Sie beides getrennt erfassen können:

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` legt fest, wie viel protokolliert wird. Ein Release-Build protokolliert
  standardmäßig auf `info`, ein Debug-Build auf `debug`. Verwenden Sie `RUST_LOG=debug`, wenn Sie
  ein Problem reproduzieren, und `RUST_LOG=trace` nur auf Nachfrage: Es wird sehr umfangreich.
- Das Logformat des Servers wird über `LOG_FORMAT` festgelegt: standardmäßig `pretty` in
  Debug-Builds, `json` in Release-Builds ([](/development/local-development/#logs)).
- Die Vollbild-TUI (ein bloßes `capsule` in einem Terminal) kann nicht auf den Bildschirm
  protokollieren, den sie zeichnet. Sie schreibt deshalb `tui.log` in das Cache-Verzeichnis der
  CLI, das `capsule status` als Cache Directory ausgibt. Die Datei wird in jeder Sitzung
  überschrieben, kopieren Sie sie also, bevor Sie die TUI erneut starten.
- Die `--json`-Ausgabe ist die genaueste Aufzeichnung dessen, was ein Befehl getan hat, weil sie
  das Ergebnis jedes Assets meldet. Hängen Sie sie an, wenn ein Befehl Fehler meldet.

**Schwärzen Sie vor dem Teilen.** Entfernen Sie Passphrasen, Wiederherstellungsgeheimnisse,
Tokens, Schlüssel, Serveradressen und persönliche Dateipfade. Fügen Sie eine
Bibliotheks-Passphrase oder ein Wiederherstellungsgeheimnis niemals irgendwo ein, auch nicht in
einen vertraulichen Bericht.

<a id="the-server"></a>

## Der Server

<a id="it-refuses-to-start"></a>

### Er startet nicht

- **Exit-Status 2 mit Variablennamen.** Ein dauerhafter Server braucht `DATABASE_URL`,
  `BLOB_ROOT`, `JWT_ED25519_DER` und `ATTESTATION_KEY_SEED` und nennt alle fehlenden auf einmal.
  Siehe [](/guides/self-hosting/#configure).
- **Das Datenbankschema ist älter als das Binary.** `serve` migriert nie. Führen Sie bei jedem
  Upgrade vor `serve` `capsule-server-migration up` aus
  ([](/guides/self-hosting/#migrate-then-start)).
- **Eine Einstellung für vertrauenswürdige Proxys wird abgelehnt.** `TRUSTED_PROXIES` lehnt ein
  `/0`-Netz ab, und `TRUSTED_PROXY_HEADER` akzeptiert nur `x-forwarded-for` oder `forwarded`.

Ein laufender Server gibt `listening on <url>` auf stdout aus.

<a id="clients-cant-sign-in-or-connect"></a>

### Clients können sich nicht anmelden oder verbinden

- `capsule server-info` zeigt, wie der Server Benutzer anmeldet: ob er einen Identitätsanbieter
  hat und ob er Passwörter ablehnt. Eine Passwortanmeldung bei einem Server mit deaktivierten
  Passwörtern schlägt absichtlich fehl.
- Hinter einem Reverse Proxy teilen sich alle Clients das Ratenlimit des Proxys, bis der Proxy in
  `TRUSTED_PROXIES` oder `TRUSTED_PROXY_HOPS` eingetragen ist. Wenn mehrere Personen gleichzeitig
  ausgebremst werden, prüfen Sie das zuerst ([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### Index und Blob-Speicher stimmen nicht überein

`capsule-server scrub` ist eine schreibgeschützte Integritätsprüfung und endet bei jedem Befund mit
einem Exit-Code ungleich null. Nachdem Sie ein Backup wiederhergestellt oder die Datenbank verloren
haben, vergleicht `capsule-server rebuild` den Index mit den Ledgern der Eigentümer. Es ändert
nichts, bis Sie `serve` stoppen und `capsule-server rebuild --apply` ausführen
([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## Importe und Uploads

- **Ein `push` ist mittendrin stehen geblieben.** Führen Sie ihn erneut aus. Uploads setzen dort
  fort, wo sie unterbrochen wurden, daher ist ein erneuter Lauf immer sicher. `push` endet mit
  einem Exit-Code ungleich null, wenn ein Asset fehlgeschlagen ist, und `--json` listet das
  Ergebnis jedes Assets auf ([](/guides/user-guide/#import-and-back-up)).
- **Behalten Sie Ihre Originale, bis die Verwahrung bestätigt ist.** Löschen Sie eine Quelldatei
  erst, wenn `push` ihr Asset als verifiziert meldet, oder verwenden Sie `import --move --push`,
  das jede Quelle erst löscht, nachdem ihr Asset verifiziert ist.
- **Ein Google-Fotos-Export wirkt unvollständig.** Prüfen Sie, ob alle Teile des Takeout-Exports
  heruntergeladen und entpackt wurden, und lesen Sie, was der Importer nicht garantieren kann
  ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Aufnahmezeiten sind falsch.** `capsule repair capture-time` meldet jedes Asset, dessen
  signierte Aufnahmezeit nicht mit den EXIF-Daten des Originals übereinstimmt, und `--apply`
  korrigiert sie. Jede Korrektur ist ein dauerhafter signierter Eintrag, führen Sie den Befehl also
  zuerst ohne `--apply` aus.

<a id="check-a-librarys-integrity"></a>

## Die Integrität einer Bibliothek prüfen

Wenn Fotos zu fehlen oder beschädigt zu sein scheinen, **halten Sie inne und löschen Sie nichts**,
während Sie der Sache nachgehen: Löschen kann den Kontext entfernen, den Sie zur Wiederherstellung
brauchen.

1. `capsule verify --library <path>` fragt den Server, ob er jedes Original dauerhaft speichert.
   Mit `--deep` hasht der Server die gespeicherten Bytes neu, statt nur ihr Vorhandensein zu
   prüfen, und `--signed` legt das signierte Urteil des Servers in der Bibliothek ab.
2. Assets im Papierkorb bleiben während ihrer Aufbewahrungsfrist wiederherstellbar:
   `capsule trash restore --library <path> --asset <id>` holt eines zurück
   ([](/guides/user-guide/#delete-safely)).
3. Für ein verlorenes Gerät oder einen verlorenen Schlüssel gibt es eigene Verfahren:
   [](/design/backup-recovery/#recovery-paths).

Wenn Daten verloren oder beschädigt sind, melden Sie es als Fehler und fügen Sie die Ausgabe jedes
Schritts bei. Datenverlust oder -beschädigung ist ein Defekt, ganz gleich, was die Ursache ist.

<a id="still-stuck"></a>

## Immer noch nicht weiter?

[](/support/) zeigt, wo Sie eine Frage stellen können und wie Sie einen Fehlerbericht verfassen,
mit dem sich arbeiten lässt.
