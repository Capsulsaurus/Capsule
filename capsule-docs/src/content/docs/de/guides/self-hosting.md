---
title: Self-Hosting
description: capsule-server mit PostgreSQL und einem Blob-Speicher im Dateisystem betreiben
status: draft
---

Es wurde noch kein Release veröffentlicht, daher bauen Sie den Server aus dem Quellcode. Der
Server speichert nur Chiffretext und dekodiert nie Medien; die Clients übernehmen die gesamte
Verarbeitung und Verschlüsselung.

<a id="what-you-need"></a>

## Was Sie brauchen

- **PostgreSQL 18**: der dauerhafte Index (Konten, Assets, Kontingente) und der ablaufende
  Zustand des Servers (Anmeldesitzungen, Upload-Sitzungen, Ratenzähler). Es ist die einzige
  Datenbank, die der Server verwendet, und sie muss mit unveränderten Dauerhaftigkeitseinstellungen
  laufen ([](#postgresql)).
- **Ein Blob-Verzeichnis** (`BLOB_ROOT`) auf **einem einzigen POSIX-Dateisystem**, das `fsync`
  einhält und Dateisperren teilt. Es enthält die Blobs und unter `ledger/` das signierte Ledger
  jedes Eigentümers, aus dem sich der Index neu aufbauen lässt ([](#blob-verzeichnis)).
- **Ein Reverse Proxy, der TLS terminiert**, vor dem Server, der Komprimierung unverändert
  durchreicht ([](#reverse-proxy)).
- **Eine synchronisierte Uhr** auf dem Host, per NTP gehalten ([](#uhr)).

### PostgreSQL

Der Server beantwortet einen Schreibvorgang, sobald dessen Zeile committet ist. Ein Commit, den
PostgreSQL bestätigt und dann verliert, kann daher eine widerrufene Sitzung zurückbringen oder
einen Code für den zweiten Faktor zweimal akzeptieren. Behalten Sie bei:

- `fsync = on` und `full_page_writes = on`, die Standardwerte;
- `synchronous_commit` auf `on`, `local`, `remote_write` oder `remote_apply`, niemals `off`;
- das Datenverzeichnis auf Speicher, der einen Flush einhält. Ein Festplatten-Schreibcache ohne
  Schutz bei Stromausfall oder eine virtuelle Festplatte, deren Host Flushes ignoriert, verliert
  bei einem Stromausfall bestätigte Commits ([](/design/filesystem/server/)).

PostgreSQL darf auf einem anderen Dateisystem, einer anderen Festplatte oder einem anderen Host
liegen als `BLOB_ROOT`.

<a id="blob-directory"></a>

### Blob-Verzeichnis

`BLOB_ROOT` ist ein Vertrag mit dem Dateisystem
([](/design/filesystem/server/#blob-root-filesystem-contract)), aufgebaut wie in
[](/design/filesystem/server/#blob-store-layout) beschrieben:

- **Ein Dateisystem.** Das Finalisieren ist ein atomares Umbenennen, daher darf der gesamte Baum,
  `incoming/` eingeschlossen, nicht über mehrere Volumes, Datasets oder Kubernetes
  PersistentVolumes verteilt sein.
- **`fsync` wird eingehalten**, für Dateien und für Verzeichnisse, bevor der Server einen
  Schreibvorgang bestätigt.
- **Dateisperren werden geteilt** von jedem Prozess, der in den Baum schreibt. `serve` und
  `purge --apply` sperren vor dem Anhängen das `ledger/<owner>/.lock` des jeweiligen Eigentümers
  ([](/design/filesystem/server/#concurrent-appenders)).
- **Ein `serve` pro Blob-Verzeichnis.** Upload-Chunks und Finalisierung werden im bedienenden
  Prozess beansprucht ([](/design/transfer/upload/)), daher werden zwei `serve`-Prozesse auf einem
  Baum, ob auf einem Host oder auf zwei, nicht unterstützt. `gc`, `purge` und `scrub` dürfen
  daneben laufen; `rebuild --apply` erfordert, dass `serve` gestoppt ist ([](#wartung)).

Unterstützt:

- ext4, XFS und btrfs auf lokalem oder Block-Speicher: eine Festplatte, RAID, LVM, iSCSI oder ein
  Cloud-Block-Volume;
- ZFS mit `sync=standard` (Standard) oder `sync=always`;
- NFSv4 oder NFSv3 mit seinem Lock-Manager, exportiert mit `sync`;
- ein Kubernetes PersistentVolume oder ein Container-Volume auf Basis eines der oben genannten.

Nicht unterstützt:

- ein Mount, der Write Barriers deaktiviert, oder eine virtuelle Festplatte, deren Host Flushes
  ignoriert;
- ZFS mit `sync=disabled`, das `fsync` bestätigt, ohne zu schreiben;
- NFS, exportiert mit `async`, das Schreibvorgänge bestätigt, die der NFS-Server nicht gespeichert
  hat, oder gemountet mit `nolock`, `local_lock=flock` oder `local_lock=all`, die Sperren auf einen
  Host beschränken;
- SMB/CIFS-Freigaben;
- FUSE-Dateisysteme (sshfs, rclone mount, s3fs, gcsfuse) und jeder Objektspeicher-Mount;
- Pooling- und Union-Dateisysteme, die einen Baum über mehrere Festplatten verteilen (mergerfs,
  Unraid-Benutzerfreigaben unter `/mnt/user`): Ein Umbenennen zwischen Mitgliedsfestplatten ist
  eine Kopie, kein atomares Umbenennen;
- die eigene beschreibbare Schicht eines Containers sowie ein macOS- oder Windows-Host-Verzeichnis,
  das per Bind-Mount in eine Container-VM eingebunden ist. Verwenden Sie ein Volume innerhalb der
  VM.

Unter Linux wird `flock` über NFS mit Byte-Range-Sperren emuliert, die der Lock-Manager zwischen
Hosts teilt. Ist das Sperren deaktiviert, können `serve` und `purge --apply` auf verschiedenen
Hosts ihre Anhängevorgänge an ein Ledger verschränken.

- **Machen Sie einen Stromausfall-Test, bevor Sie der Hardware vertrauen.** `serve` prüft beim
  Start den Dateisystemtyp, das atomare Umbenennen und das Teilen von Sperren, aber keine
  Softwareprüfung erkennt eine Festplatte, einen Controller oder einen Hypervisor, der einen
  Flush bestätigt, den er nicht ausgeführt hat. Unterbrechen Sie auf der eingesetzten Hardware
  während eines Uploads die Stromversorgung (kein sauberes Herunterfahren), starten Sie dann neu
  und führen Sie `capsule-server scrub --deep` aus: Jeder Upload, den der Server bestätigt hat,
  muss intakt sein ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Lassen Sie freien Speicherplatz.** Das Blob-Root hält eine Reserve an freiem Speicherplatz
  vor, standardmäßig den größeren Wert aus 2 % des Volumes und 4 GiB. Darunter werden neue Uploads
  mit `507` abgelehnt, während Uploads, die schon auf dem Server laufen, abgeschlossen werden und
  Löschvorgänge weiterhin funktionieren ([](/design/quota/#enforcement-points)). Die Reserve ist
  geplant und noch nicht umgesetzt; bis dahin lehnt eine volle Festplatte jeden Schreibvorgang ab.
- **Bereinigen Sie die Quarantäne bewusst.** `quarantine/` bewahrt Blobs, deren erneutes Hashen
  fehlgeschlagen ist oder die nicht gelesen werden konnten, so lange auf, bis Sie sie löschen,
  außer denen, die inzwischen repariert wurden. Das geplante `capsule-server quarantine prune`
  listet vor dem Entfernen auf und entfernt ohne `--apply` nichts
  ([](/design/filesystem/server/#quarantine-retention)). `scrub` meldet die Größe der Quarantäne,
  behalten Sie sie also dort im Blick.

### Reverse Proxy

TLS wird am Proxy terminiert, niemals am Server ([](/design/networking/#transport-security)). Der
Server lauscht auf einfachem HTTP und darf nur vom Proxy aus erreichbar sein: über Loopback, ein
privates Netz oder ein Pod-Netz. Konfigurieren Sie den Proxy so, dass er:

- Request-Bodys von mindestens 16 MiB annimmt, der Größe des größten Upload-Chunks;
- Request-Bodys an den Server streamt, statt sie zu puffern;
- TLS Early Data (0-RTT) deaktiviert oder mit dem Header `Early-Data: 1` weiterleitet, damit der
  Server einen Schreibvorgang ablehnen kann;
- für einen Request-Body kein Gesamtdauer-Timeout anwendet und ein Leerlauf-Timeout, das länger
  ist als die Stillstandsgrenze des Servers, damit ein langsamer, aber stetiger Chunk nie
  abgeschnitten wird;
- **Komprimierung durchreicht.** Der Server entscheidet, welche Antworten komprimiert werden, und
  komprimiert `application/octet-stream` nie ([](/design/networking/#compression)). Der Proxy
  reicht Capsules Upstream-Antworten so durch, wie der Server sie gesendet hat: Er DARF eine
  Antwort NICHT (MUST NOT) mit gzip oder anders komprimieren, DARF keine dekomprimieren und
  `Content-Encoding` NICHT hinzufügen, ändern oder entfernen (MUST NOT), und MUSS (MUST) das
  `Accept-Encoding` des Clients unverändert weiterleiten. Behalten Sie in nginx für die
  Capsule-Site `gzip off;` bei, was ein aus der `nginx.conf` der Distribution geerbtes `gzip on;`
  überschreibt. Fügen Sie in Caddy keine `encode`-Direktive hinzu; hängen Sie in Traefik keine
  `compress`-Middleware an.

HTTP/2 und HTTP/3 auf der Client-Seite des Proxys sind optional; Clients weichen auf HTTP/1.1
aus. Kernel-TLS (kTLS) und TLS-Offload auf die Netzwerkkarte sind, falls gewünscht,
Proxy-Einstellungen (in nginx `ssl_conf_command Options KTLS;` mit `sendfile on;`). Der eigene
TLS-Stack des Servers wird nur für die Verbindungen verwendet, die er selbst aufbaut: zu einem
OpenID-Connect-Anbieter und zu föderierten Peers. Diese vertrauen den mitgelieferten
Mozilla-Root-Zertifikaten sowie einem etwaigen `OIDC_CA_BUNDLE`, nicht dem Zertifikatsspeicher
des Hosts.

<a id="clock"></a>

### Uhr

Der Server beurteilt die Zeit nach seiner eigenen Uhr, und mehrere Regeln hängen davon ab, dass
diese Uhr stimmt. Halten Sie sie auf dem Host per NTP (chrony oder systemd-timesyncd)
synchronisiert. Ein Container liest die Uhr des Hosts, also synchronisieren Sie den Host. Die
Abweichung SOLLTE (SHOULD) unter einer Sekunde bleiben und MUSS (MUST) unter 30 Sekunden bleiben.
Jenseits dieser Grenzen:

| Abweichung | Was nicht mehr funktioniert |
| --- | --- |
| Über 30 s | Codes für den zweiten Faktor schlagen fehl: Ein Code wird nur innerhalb eines 30-s-Schritts der Serveruhr akzeptiert ([](/design/authentication/)) |
| Über 5 min | Die Föderation stoppt in beide Richtungen: Peers lehnen die signierten Anfragen, Hinweise und Berichte dieses Servers ab, und er lehnt ihre ab ([](/design/federation/)) |
| Uhr geht vor | Der Papierkorb wird vor Ende seiner Aufbewahrungsfrist geleert, und eine rechtzeitige Wiederherstellung wird abgelehnt ([](/design/organization/#retention-window)). Sitzungen, Freigabelinks und Upload-Sitzungen laufen zu früh ab |
| Uhr geht nach | Abgelaufene Sitzungen, Freigabelinks und Zähler bleiben länger gültig als konfiguriert |
| Grob falsch | Clients lehnen Verwahrungsbelege ab, deren `received_at` unplausibel ist ([](/design/transfer/durability/)) |

Starten Sie `serve` erst, wenn die Uhr synchronisiert ist. Ordnen Sie es unter systemd nach
`time-sync.target` ein, mit aktiviertem `chrony-wait.service` oder
`systemd-time-wait-sync.service`. Eine aus einem Snapshot wiederhergestellte virtuelle Maschine
läuft mit der Uhrzeit des Snapshots weiter: Lassen Sie NTP die Uhr stellen, bevor `serve` startet.
Der Server erkennt oder meldet Uhrabweichungen noch nicht selbst.

## Hardware

Diese Angaben gelten für einen Knoten, auf dem Server und PostgreSQL gemeinsam laufen. Sie sind
Dimensionierungshinweise, abgeleitet aus dem, was der Server tut, keine Benchmarks.

Die Arbeit des Servers ist leicht, daher sollte der Großteil des Hardwarebudgets in den
**Speicher** fließen. Er hasht jeden Chunk beim Eintreffen mit SHA-256 und hasht beim Finalisieren
und während `scrub --deep` erneut. Er prüft beim Finalisieren die hybride Signatur des Clients
(Ed25519 und ML-DSA-65), signiert Belege auf dieselbe Weise und führt pro Passwortanmeldung einmal
Argon2id aus (jeweils etwa 19 MiB). Er dekodiert, skaliert oder transkodiert nie Medien,
verschlüsselt oder entschlüsselt nie und überlässt TLS dem Reverse Proxy. Der Body eines Uploads
wird nie über einen Chunk hinaus gepuffert, daher wächst der Speicherbedarf nicht mit der
Dateigröße.

| | Minimum | Empfohlen |
| --- | --- | --- |
| **CPU** | 2 Kerne, x86_64 oder arm64 | 4 Kerne. SHA-2-Befehle (SHA-NI auf x86_64, die Armv8-Kryptografieerweiterungen) beschleunigen das Hashen |
| **RAM** | 2 GB | 4 bis 8 GB. Mehr hilft dem Cache von PostgreSQL bei einer großen Bibliothek, aber Speicher zahlt sich zuerst aus |
| **Systemspeicher** | SSD, 10 GB frei | SSD oder NVMe für PostgreSQL |
| **Blob-Speicher** | Die Größe Ihrer Bibliothek plus Platz für laufende Uploads | Redundante Festplatten (Spiegel, RAID oder ZFS), ausgelegt auf mehrere Jahre Wachstum. Klassische Festplatten genügen |
| **Netzwerk** | 10 Mbit/s Upstream | Gigabit im LAN und 50 Mbit/s oder mehr Upstream, wenn Geräte von außerhalb synchronisieren |
| **Stromversorgung** | — | Eine USV, damit ein Stromausfall PostgreSQL nicht mitten im Schreiben unterbricht |

- **`BLOB_ROOT` bleibt auf einem Dateisystem.** Der gesamte Baum, `incoming/` eingeschlossen,
  muss ein Dateisystem sein. Er lässt sich also nicht auf eine SSD und ein Festplatten-Array oder
  auf zwei ZFS-Datasets aufteilen. Legen Sie alles auf das Array.
- **Geben Sie PostgreSQL den Arbeitsspeicher, den der Server nicht nutzt.** Jede authentifizierte
  Anfrage liest ihre Sitzung aus PostgreSQL, und jeder Upload-Chunk vermerkt dort seinen Bereich.
  Der Cache bedient also den Anfragepfad ebenso wie den Bibliotheksindex. Sitzungen,
  Upload-Sitzungen und Zähler sind klein, und der Server löscht sie, sobald sie abgelaufen sind.
- **Die Upstream-Bandbreite ist die Grenze für entfernte Geräte.** Ein Smartphone außerhalb Ihres
  Netzes lädt mit der *Download*-Geschwindigkeit Ihres Anschlusses hoch, aber jeder Abruf einer
  Vorschau oder eines Originals verlässt das Haus mit Ihrer *Upstream*-Geschwindigkeit. Ein Chunk
  wird nur abgeschnitten, wenn er nicht mehr ankommt, nie, weil er lange dauert, und ein Client
  bemisst seine Chunks so, dass sie bei der gemessenen Geschwindigkeit in einer kurzen Zielzeit
  fertig werden. Eine langsame Leitung lädt also in ihrem eigenen Tempo hoch. Halten Sie die
  Stillstandsgrenze des Servers unter dem Leerlauf-Timeout Ihres Proxys, damit der Server auf einen
  hängenden Chunk antwortet, bevor der Proxy ihn abschneidet.
- **Dimensionieren Sie die Festplatten für Originale, nicht für Ableitungen.** Ableitungen machen
  bei einer Bibliothek aus RAW-Dateien und Videos nur einen kleinen Bruchteil aus. Der Server
  speichert jeden Blob einmal, so wie der Client ihn verschlüsselt hat.
- **Mehr Benutzer bedeuten mehr Bandbreite, nicht mehr Rechenleistung.** Suche, Vorschaubilder und
  Ordnung laufen auf den Clients. Mehr Menschen, die stöbern, bedeuten mehr ausgelieferte Bytes,
  aber kaum mehr CPU oder RAM.

`capsule-server/compose.yaml` startet Postgres mit den richtigen Einstellungen für den lokalen
Gebrauch (`mise run serve-deps`). Die Zugangsdaten darin sind nur für die Entwicklung gedacht.

<a id="build"></a>

## Bauen

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

Die Binaries sind gegen glibc gelinkt und brauchen sonst nichts Installiertes. Führen Sie sie als
unprivilegierter Benutzer aus, dessen einziger beschreibbarer Pfad `BLOB_ROOT` ist, also in
derselben Form, die das Container-Image unten erzwingt.

## Container-Image

:::caution
Es ist noch kein Image veröffentlicht, und die Routen `/livez` und `/readyz` sowie der
Unterbefehl `healthcheck` sind noch nicht umgesetzt. Dieser Abschnitt ist der Vertrag, nach dem
das Image gebaut wird.
:::

- **Basis:** `gcr.io/distroless/cc-debian12:nonroot`, per Digest fixiert. Es enthält glibc, die
  GCC-Laufzeit, CA-Zertifikate und Zeitzonendaten, aber keine Shell und keinen Paketmanager.
- **Architekturen:** ein Multi-Arch-Image für `linux/amd64` und `linux/arm64`, jeweils nativ für
  sein glibc-Ziel gebaut. Die Builds sind nicht auf eine bestimmte CPU abgestimmt; das Hashen wählt
  zur Laufzeit SHA-NI oder die Armv8-Kryptografieerweiterungen.
- **Inhalt:** `capsule-server` (der Entrypoint, standardmäßig `serve`) und
  `capsule-server-migration` aus demselben Build.
- **Benutzer:** kein Root, UID und GID 65532. Der Container braucht keine Linux-Capabilities:
  Entziehen Sie alle und setzen Sie `no-new-privileges`.
- **Schreibgeschütztes Root-Dateisystem.** Der Server schreibt nur unter `BLOB_ROOT`. Mounten Sie
  das Blob-Verzeichnis also als einziges beschreibbares Volume, im Besitz von UID 65532, und das
  Root-Dateisystem schreibgeschützt.
- **Listener:** Setzen Sie im Container `SERVER_HOST=0.0.0.0` (oder `::`); Port 3000 ist der
  Standard. Veröffentlichen Sie ihn nur für den Reverse Proxy ([](#reverse-proxy)).
- **Logs** gehen nach stderr; setzen Sie `LOG_FORMAT=json` für einen Log-Collector.
- **Stoppen:** `serve` fährt bei `SIGTERM` geordnet herunter. Ein durch das Stoppen unterbrochener
  Upload setzt ab den auf dem Server committeten Bereichen fort.
- **Eine Replica.** Führen Sie pro Blob-Verzeichnis genau ein `serve` aus ([](#blob-verzeichnis)).
  Verwenden Sie auf Kubernetes eine Replica mit der Update-Strategie `Recreate`: Ein Rolling
  Update startet den neuen Server, während der alte noch bedient.
- **Migrationen** laufen aus demselben Image-Tag, bevor `serve` startet, als Job, Init-Container
  oder `ExecStartPre=`: `capsule-server-migration up` ([](#migrieren-dann-starten)).
- **Wartungsbefehle** laufen aus demselben Image, als derselbe Benutzer, auf demselben Volume, als
  CronJobs oder Timer ([](#wartung)).
- **Health:** `/livez` beantwortet ein `GET`, solange der Prozess bedient, und prüft sonst
  nichts – für eine Liveness-Probe. `/readyz` antwortet nur, solange PostgreSQL antwortet, das
  Schema zum Binary passt und `BLOB_ROOT` beschreibbar ist – für eine Readiness-Probe. Beide
  liegen außerhalb von `/v1`, sodass eine Probe nicht von der Protokollversion abhängt, und keine
  muss über den Proxy erreichbar sein. Das Image enthält kein `curl`, daher führt sein eigener
  Health-Check `capsule-server healthcheck` aus, das `/livez` auf dem lokalen Listener aufruft.
- **Uhr:** Der Container liest die Uhr des Hosts; synchronisieren Sie den Host ([](#uhr)).

<a id="configure"></a>

## Konfiguration

Die Konfiguration erfolgt ausschließlich über Umgebungsvariablen. `capsule-server/.env.example`
ist eine Startvorlage, und diese Seite nennt die Variablen, über die ein Betreiber entscheiden
muss. Ein dauerhafter Server verweigert den Start (Exit-Code 2) und nennt alle fehlenden Variablen
auf einmal, wenn Folgendes fehlt:

| Variable | Zweck |
| --- | --- |
| `DATABASE_URL` | PostgreSQL-Verbindung |
| `BLOB_ROOT` | Blob-Verzeichnis |
| `JWT_ED25519_DER` | Base64-codierter Ed25519-Schlüssel, der Sitzungstokens signiert |
| `ATTESTATION_KEY_SEED` | Base64-codierter Seed, der Verwahrungsbelege signiert; muss unabhängig vom Token-Schlüssel sein |

- **Schützen Sie vor allem den Token-Schlüssel.** Die Datenbank enthält kein Bearer-Geheimnis, eine
  geleakte Datenbank oder ein geleaktes Backup liefert also für sich allein keine Sitzung; erst
  `JWT_ED25519_DER` macht aus einem solchen Leak Zugriff auf jede aktive Sitzung
  ([](/design/authentication/#session-token)). Die Erkennung wiederverwendeter Refresh-Tokens
  ([](/design/authentication/#token-lineage-and-reuse)) hilft dagegen nicht, weil jemand, der den
  Schlüssel besitzt, Tokens selbst ausstellt, statt sie wiederzuverwenden. Bewahren Sie ihn in
  Ihrem Secret Store auf, niemals in der Datenbank oder deren Backups, und rotieren Sie ihn nach
  einem vermuteten Leak, wodurch jedes Gerät abgemeldet wird.
- **Ein Versiegelungsschlüssel für den zweiten Faktor ist geplant.** Der Server wird TOTP-Geheimnisse
  im Ruhezustand unter einem eigenen Schlüssel versiegeln, unabhängig vom Token-Schlüssel, sodass
  ein Leak der Datenbank oder eines Backups keinen zweiten Faktor preisgibt und das Rotieren des
  Token-Schlüssels keinen ungültig macht ([](/design/authentication/#the-second-factor)). Die
  Einstellung dafür gibt es noch nicht; wenn sie kommt, halten Sie den Schlüssel wie den
  Token-Schlüssel getrennt von der Datenbank und ihren Backups.
- **Offene Registrierung braucht ein Standardkontingent** (geplant). Sobald Kontingente
  konfigurierbar sind, verweigert `serve` den Start, wenn die Passwortregistrierung aktiviert ist
  und kein Standardlimit gesetzt ist. Ein Betreiber, der keines will, setzt es ausdrücklich auf
  `unlimited`, was bei jedem Start auf `WARN` protokolliert wird
  ([](/design/quota/#thresholds-and-states)). Heute läuft jede Installation unbegrenzt.

`SYNC_CURSOR_MAC_KEY` wird aus `JWT_ED25519_DER` abgeleitet, wenn es nicht gesetzt ist. Das
Rotieren des Token-Schlüssels macht daher auch ausstehende Sync-Cursor ungültig, sofern Sie ihn
nicht ausdrücklich setzen. Die Föderation ist aus, solange `FEDERATION_ENABLED=true` und
`FEDERATION_URL` nicht gesetzt sind; lassen Sie sie aus.

Benennen Sie Ihren Reverse Proxy, damit die quellenbezogenen Ratenlimits den Client sehen und
nicht den Proxy.

| Variable | Bedeutung |
| --- | --- |
| `TRUSTED_PROXIES` | Kommagetrennte Adressen und CIDR-Netze (`10.0.0.0/8`). Ein Netz `/0` wird abgelehnt, weil es jedem Absender glauben würde. |
| `TRUSTED_PROXY_HOPS` | Vertraut so vielen Weiterleitungs-Hops, die dem Server am nächsten sind, unabhängig von ihrer Adresse. |
| `TRUSTED_PROXY_HEADER` | Das eine Weiterleitungsfeld, das der Server liest: `x-forwarded-for` (Standard) oder `forwarded` (RFC 7239). Jeder andere Wert verhindert den Start. |

Ohne diese Einstellungen wird keinem Weiterleitungs-Header geglaubt. Das ist sicher, aber dann
teilen sich alle Clients hinter dem Proxy dessen Kontingent, sodass ein einzelner Fremder, der
Passwörter errät, die Anmeldung für alle verlangsamt ([](/design/authentication/#sign-in-limits));
der Server protokolliert eine Warnung, wenn er einen Weiterleitungs-Header sieht, aber keinem Hop
vertraut. Der umgekehrte Fehler ist schlimmer: Wer einem Hop vertraut, der die Adresse des Clients
nicht anhängt, lässt jeden Client seinen eigenen Ratenlimit-Schlüssel wählen. Das Feld, das
`TRUSTED_PROXY_HEADER` nicht nennt, wird ignoriert, egal wer es sendet, sodass ein Client seinen
Ratenlimit-Schlüssel nicht wählen kann, indem er ein Feld sendet, das Ihr Proxy durchreicht.
Nennen Sie das Feld, an das Ihr Proxy anhängt. Lassen Sie Ihren Proxy beim Standardwert die
Adresse, die er gesehen hat, an `X-Forwarded-For` anhängen, wie es nginx hiermit tut:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Setzen Sie `TRUSTED_PROXY_HEADER=forwarded` nur, wenn Ihr Proxy sein eigenes Element an
`Forwarded` anhängt und `X-Forwarded-For` nicht schreibt.

<a id="migrate-then-start"></a>

## Migrieren, dann starten

`serve` migriert nie und lehnt ein Datenbankschema ab, das älter ist als das Binary. Führen Sie
die Migrationen zuerst aus, bei jedem Upgrade:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` protokolliert nach stderr und gibt `listening on <url>` auf stdout aus. Verwenden Sie
`--memory` nicht in Produktion: Damit liegt alles außer den Blobs im Prozessspeicher.

<a id="maintenance"></a>

## Wartung

Planen Sie diese einmaligen Befehle ein (Cron oder ein Timer). Sie brauchen `DATABASE_URL` und
`--blob-root` und laufen standardmäßig als Probelauf (Dry Run):

- `capsule-server gc --apply`: gibt nach einer Karenzzeit nicht referenzierte Blobs frei und
  löscht abgelaufene Sitzungen, Upload-Sitzungen und Zähler. Führen Sie es regelmäßig aus, auch
  wenn der Speicherplatz nicht knapp ist.
- `capsule-server purge --apply`: löscht weich gelöschte Assets endgültig, deren Aufbewahrungsfrist
  abgelaufen ist. Es braucht außerdem das `ATTESTATION_KEY_SEED` des Servers, weil jede endgültige
  Löschung zuerst signiert in das Ledger des Eigentümers
  ([](/design/filesystem/server/#the-owner-ledger)) eingetragen wird, und verweigert ohne es den
  Dienst. Es endet mit einem Exit-Code ungleich null, wenn das Ledger eine Löschung nicht
  aufzeichnen konnte; dieses Asset bleibt für den nächsten Lauf erhalten.
- `capsule-server scrub [--deep]`: schreibgeschützte Integritätsprüfung des Index gegen den
  Blob-Speicher. Es **endet bei jedem Befund mit einem Exit-Code ungleich null**, lösen Sie also
  darauf einen Alarm aus ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

Nachdem Sie ein Backup wiederhergestellt oder die Datenbank verloren haben, bauen Sie den Index
aus den Ledgern der Eigentümer unter `BLOB_ROOT` neu auf ([](/design/filesystem/server/#rebuild)).
Setzen Sie `ATTESTATION_KEY_SEED` auf dem Host, damit er die Signaturen der Ledger prüft.

1. `capsule-server rebuild` vergleicht den Index mit den Ledgern.
2. Bei gestopptem `serve` stellt `capsule-server rebuild --apply` die Zeilen wieder her, die dem
   Index fehlen. Es überschreibt nie eine Zeile, die der Index enthält. Ein Ledger, dessen Ende
   verloren gegangen ist, wird neu verankert, danach schreibt der Server wieder für diesen
   Eigentümer.
3. Meldet der Bericht, dass ein Ledger umgeschrieben wurde, lehnt der Server die Schreibvorgänge
   dieses Eigentümers ab, bis Sie der Sache nachgegangen sind und
   `capsule-server rebuild --apply --accept-rewrites` ausgeführt haben.

Ein Server, der Lebenszyklus-Operationen (Löschen, Wiederherstellen, Metadaten-Update, Ableitung,
Ersetzen) angewendet hat, bevor er die Größen ihrer Blobs aufzeichnete, führt diese Referenzen mit
Größe 0. Führen Sie einmal `capsule-server repair-sizes` aus, um sie aufzulisten, und dann
`capsule-server repair-sizes --apply`, um jede Größe aus dem Blob-Speicher nachzutragen. Das ist
neben einem laufenden `serve` sicher, und jedes reparierte Asset wird erneut an die Clients
gesendet. Es endet mit einem Exit-Code ungleich null, wenn ein Blob fehlt oder leer ist, und lässt
diese Referenz unverändert.

## Backups

Der dauerhafte Zustand besteht aus PostgreSQL und `BLOB_ROOT` (Blobs und `ledger/`); sichern Sie
beides, und zwar zusammen. Die Datenbank enthält auch Anmeldesitzungen und laufende
Upload-Sitzungen, sodass Benutzer nach einer Wiederherstellung angemeldet bleiben. Bewahren Sie
auch die oben genannten Schlüsselvariablen in Ihrem Secret Store auf: Sie liegen nicht in der
Datenbank, und die Signatur- und Attestierungsidentität des Servers hängt von ihnen ab.

Sichern Sie zuerst die Datenbank, dann die Blobs:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` darf weiterlaufen. Ein Blob liegt auf der Festplatte und sein Ledger-Eintrag ist
angehängt, bevor die Indexzeile committet wird, die ihn nennt. Eine nach dem Dump erstellte Kopie
der Blobs enthält also jeden Blob, auf den der Dump verweist, und möglicherweise Schreibvorgänge,
die dem Dump entgangen sind. Führen Sie während des Backups weder `gc --apply` noch
`purge --apply` aus: Beide können einen Blob entfernen, auf den der Dump noch verweist.

Der Neuaufbau unten stellt die Assets wieder her, die zwischen dem Dump und der Blob-Kopie
hochgeladen wurden, weil er jede Zeile hinzufügt, die dem Index fehlt. Er ändert nie eine Zeile,
die der Dump bereits enthält. Ein Löschen, Wiederherstellen oder eine Metadatenbearbeitung an
einem älteren Asset in diesem Zeitfenster wird daher nicht übernommen. Ein Neuaufbau meldet eine
solche Änderung nur dann als abweichende Zeile (und endet mit einem Exit-Code ungleich null), wenn
sie die Blobs des Assets geändert hat; ein Löschen oder Wiederherstellen bleibt ungemeldet. Wenn
ein Backup jede Änderung erfassen muss, stoppen Sie `serve`, während Sie beide Hälften sichern.
Meldet eine Wiederherstellung abweichende Zeilen und läuft der alte Host noch, erstellen Sie das
Backup auf diese Weise erneut.

<a id="restoring-onto-a-new-host"></a>

### Wiederherstellung auf einem neuen Host

1. Starten Sie ein leeres PostgreSQL und setzen Sie dieselben Werte für `JWT_ED25519_DER` und
   `ATTESTATION_KEY_SEED` wie auf dem alten Host.
2. Stellen Sie die Datenbank wieder her. Der Dump enthält das Schema und seinen Migrationsstand,
   führen Sie also nicht vorher `capsule-server-migration up` aus:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Kopieren Sie das Blob-Verzeichnis zurück nach `BLOB_ROOT`, auf ein einziges Dateisystem.
4. Führen Sie bei gestopptem `serve` `capsule-server rebuild` aus und lesen Sie den Bericht. Er
   listet die nach dem Dump hochgeladenen Assets auf; `capsule-server rebuild --apply` stellt sie
   mit ihren ursprünglichen Sequenznummern wieder her, sodass die Sync-Cursor der Clients gültig
   bleiben.
5. Führen Sie `capsule-server scrub --deep` aus. Es sollte nichts melden; bei jedem Befund endet
   es mit einem Exit-Code ungleich null.
6. Starten Sie `serve`. Sitzungen kommen mit der Datenbank zurück, ein zum Zeitpunkt des Dumps
   angemeldeter Client bleibt also angemeldet, sofern seine Sitzung nicht inzwischen abgelaufen
   ist. Bevor `serve` zu lauschen beginnt, verwirft es jede unfertige Upload-Sitzung, deren
   zwischengespeicherte Datei unter `BLOB_ROOT` nicht wiederhergestellt wurde, und löscht jede
   zwischengespeicherte Datei ohne Sitzung; ein Client, dessen Upload verworfen wurde, beginnt
   diese Datei von vorn. Die Clients setzen die Synchronisation dort fort, wo sie aufgehört haben.

Ist die Datenbank verloren und gibt es keinen Dump, migrieren Sie eine leere Datenbank und führen
Sie `capsule-server rebuild --apply` über dem Blob-Verzeichnis aus. Es stellt jede Asset-Zeile aus
den Ledgern der Eigentümer wieder her. Konten, veröffentlichte Geräteverzeichnisse,
Albummitgliedschaften und Freigabelinks liegen nur in der Datenbank, und ein Neuaufbau stellt sie
nicht wieder her.

Proben Sie diesen Ablauf, bevor Sie ihm echte Daten anvertrauen, etwa indem Sie mit
`capsule restore` prüfen, dass jedes Original Byte für Byte zurückkommt.
`capsule-e2e/tests/backup_restore_drill.rs` führt dieselben Schritte gegen Container aus
(`mise run test-rust-containers`).
