---
title: Benutzerhandbuch
description: Der tägliche Umgang mit einer Capsule-Bibliothek über die capsule-CLI — importieren, sichern, prüfen, ordnen und sicher löschen.
audit: none
---

Dieses Handbuch behandelt die `capsule`-CLI, den Referenz-Client. Mit *(geplant)* markierte
Befehle stehen auf der [Roadmap](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)
und sind noch nicht verfügbar. Maßgeblich für die Flags ist `capsule <command> --help`; die
CLI-Referenz ([](/reference/cli/)) listet sie alle auf.

:::caution
Capsule ist im Alpha-Stadium. Bewahren Sie bis zum ersten Dogfood-Release von jedem Original, das
Sie importieren, eine unabhängige Sicherung auf.
:::

<a id="set-up"></a>

## Einrichtung

1. Legen Sie eine Bibliothek an: `capsule library init --name "Photos"`. Die Bibliothek ist ein
   lokales, verschlüsseltes Verzeichnis; ihre Passphrase schützt die Schlüssel auf diesem
   Rechner.
2. Melden Sie sich bei Ihrem Server an: `capsule auth register` oder `capsule auth login`
   (`--oidc` oder `--device` für Single Sign-on).
3. Richten Sie die Wiederherstellung ein: `capsule auth escrow`. Bewahren Sie das ausgegebene
   Wiederherstellungsgeheimnis offline auf. Ohne es und ohne Ihre Passphrase bedeutet ein
   verlorener Rechner eine verlorene Bibliothek, denn der Server kann nichts entschlüsseln.

<a id="import-and-back-up"></a>

## Importieren und sichern

- `capsule import <path>` kopiert Dateien in die Bibliothek, signiert Herkunft und Metadaten
  jedes Assets und erzeugt Vorschaubilder. `--provider takeout` importiert ein
  Google-Takeout-Archiv samt seinen Sidecar-Metadaten ([](/guides/google-photos-migration/)).
- `capsule push` lädt die Bibliothek hoch. Uploads setzen dort fort, wo sie unterbrochen wurden,
  daher ist ein erneutes `push` immer sicher. Nach jedem Asset fordert `push` den Server auf zu
  bestätigen, dass er jeden Blob dauerhaft speichert, und legt die signierten Verwahrungsbelege
  des Servers in der Bibliothek ab. Ein fehlgeschlagenes Asset wird gemeldet, und der Lauf geht
  weiter. Der Befehl endet mit einem Exit-Code ungleich null, wenn ein Asset fehlgeschlagen ist,
  und `--json` listet das Ergebnis jedes Assets auf.
- `capsule import --push` erledigt beides in einem Schritt.
- **Behalten Sie Ihre Originale, bis die Verwahrung bestätigt ist.** Ein Asset ist erst dann
  sicher gespeichert, wenn der Server es als dauerhaft gespeichert meldet und einen signierten
  Verwahrungsbeleg ausgestellt hat, was `push` als verifiziert meldet. Löschen Sie Quelldateien
  nicht vorher. `import --move --push` löscht jede Quelle erst, nachdem ihr Asset verifiziert
  ist. Ohne `--push` behält `--move` alle Quellen und weist darauf hin; ein späteres
  `import --move --push` über dieselben Dateien gibt sie frei.
- `capsule restore` *(geplant)* lädt Originale herunter und prüft sie Byte für Byte gegen das,
  was beim Import signiert wurde.

<a id="organize"></a>

## Ordnen

- **Labels** sind frei wählbar und hängen an einem Asset, einem Stapel (RAW+JPEG-Paar,
  Serienaufnahme, Belichtungsreihe), einem Album oder einem Bildbereich. Von Ihnen vergebene Labels
  werden getrennt von KI-Vorschlägen gespeichert, und Sie können einen Vorschlag übernehmen oder
  verwerfen. Asset-Labels gibt es heute; Labels für Stapel, Alben und Bildbereiche sind
  *(geplant)*.
- `capsule show <asset>` gibt aus, was die signierten Metadaten eines Assets festhalten:
  Bildunterschrift, Bewertung, Labels, Ort, Aufnahmezeit.
- `capsule repair capture-time` findet Assets, deren Aufnahmezeit nicht mit den EXIF-Daten des
  Originals übereinstimmt, und korrigiert sie mit `--apply`. Das Bearbeiten des Orts ist
  *(geplant)*.

Jede Bearbeitung ist eine signierte, nur anhängende Änderung: Der frühere Wert bleibt im
Herkunftsverlauf des Assets erhalten, und gleichzeitige Bearbeitungen von zwei Geräten werden
deterministisch zusammengeführt.

<a id="delete-safely"></a>

## Sicher löschen

Korrigieren Sie Metadaten wie die Geolokalisierung, bevor Sie löschen, solange die umliegenden
Assets noch Kontext liefern. Ein Foto ohne eigenen Ort lässt sich leicht zuordnen, solange seine
Nachbarn vom selben Spaziergang in der Bibliothek sind; sind sie weg, ist es auch dieser Kontext.

- `capsule cull` markiert Assets als ausgewählt, neutral oder abgelehnt und filtert nach dieser
  Markierung.
- `capsule cull --sweep` verschiebt abgelehnte Assets in den Papierkorb. Assets im Papierkorb
  bleiben während der Aufbewahrungsfrist (`--retain-days`) wiederherstellbar, bevor sie endgültig
  gelöscht werden.
- `capsule trash delete --asset <id>` verschiebt ein Asset sowohl auf dem Server als auch in der
  Bibliothek in den Papierkorb. Ein Asset, das schon im Papierkorb liegt, etwa nach
  `capsule cull --sweep`, behält die Aufbewahrungsfrist, mit der es dorthin verschoben wurde:
  `--retain-days` gilt nur für ein neues Löschen, und `--json` meldet die übermittelte Frist als
  `retention_until`.
- `capsule trash restore --asset <id>` holt ein Asset zurück. Eine Wiederherstellung nach Ablauf
  der Aufbewahrungsfrist wird abgelehnt (`retention_elapsed` unter `--json`) und nicht wiederholt;
  das Asset bleibt im Papierkorb, bis es endgültig gelöscht wird.

<a id="share"></a>

## Teilen

- `capsule share create --album <id>` oder `--asset <id>` stellt einen Link nur zum Ansehen aus;
  `--protect` fügt eine Link-Passphrase hinzu und `--expires-at` ein Ablaufdatum. Der Link trägt
  seinen Schlüssel im URL-Fragment, das den Server nie erreicht.
- `capsule share revoke <id>` sorgt dafür, dass sich ein Link nicht mehr öffnen lässt.

<a id="sync-another-machine"></a>

## Einen weiteren Rechner synchronisieren

- `capsule sync` holt den Änderungs-Feed der Bibliothek vom Server; `capsule list` zeigt, was
  dabei angekommen ist.
- `capsule auth recover` bringt das Konto mithilfe des Wiederherstellungsgeheimnisses auf einen
  neuen Rechner.
