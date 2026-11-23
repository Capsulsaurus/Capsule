---
title: Self-hosting
description: Eseguire capsule-server su PostgreSQL e un archivio di blob sul file system
audit: none
---

Non è ancora stata pubblicata alcuna release, quindi il server si compila dal codice sorgente. Il
server memorizza solo testo cifrato e non decodifica mai i contenuti multimediali; i client si
occupano di tutta l'elaborazione e della cifratura.

<a id="what-you-need"></a>

## Cosa ti serve

- **PostgreSQL 18**: l'indice durevole (account, elementi, quota) e lo stato a scadenza del server
  (sessioni di accesso, sessioni di caricamento, contatori di frequenza). È l'unico database che il
  server usa, e deve funzionare con le impostazioni di durabilità intatte ([](#postgresql)).
- **Una directory dei blob** (`BLOB_ROOT`) su **un unico file system POSIX** che rispetti `fsync` e
  condivida i lock dei file. Contiene i blob e, sotto `ledger/`, il registro firmato di ogni
  proprietario, da cui l'indice può essere ricostruito ([](#directory-dei-blob)).
- **Un reverse proxy che termini TLS** davanti al server e che lasci passare la compressione senza
  toccarla ([](#reverse-proxy)).
- **Un orologio sincronizzato** sull'host, mantenuto da NTP ([](#orologio)).

### PostgreSQL

Il server risponde a una scrittura non appena la riga corrispondente viene confermata, quindi un
commit che PostgreSQL conferma e poi perde può riportare in vita una sessione revocata o accettare due
volte un codice del secondo fattore. Mantieni:

- `fsync = on` e `full_page_writes = on`, i valori predefiniti;
- `synchronous_commit` a `on`, `local`, `remote_write` o `remote_apply`, mai a `off`;
- la directory dei dati su uno storage che rispetti il flush. Una cache di scrittura del disco senza
  protezione contro la perdita di alimentazione, o un disco virtuale il cui host ignora i flush,
  perde commit già confermati in caso di interruzione di corrente ([](/design/filesystem/server/)).

PostgreSQL può trovarsi su un file system, un disco o un host diversi da quelli di `BLOB_ROOT`.

<a id="blob-directory"></a>

### Directory dei blob

`BLOB_ROOT` è un contratto con il file system
([](/design/filesystem/server/#blob-root-filesystem-contract)), organizzato come descritto in
[](/design/filesystem/server/#blob-store-layout):

- **Un unico file system.** La finalizzazione è una rinomina atomica, quindi l'intero albero,
  `incoming/` compresa, non può estendersi su più volumi, dataset o PersistentVolume di Kubernetes.
- **`fsync` viene rispettato**, su file e directory, prima che il server confermi una scrittura.
- **I lock dei file sono condivisi** da ogni processo che scrive nell'albero. `serve` e
  `purge --apply` acquisiscono il lock `ledger/<owner>/.lock` di ogni proprietario prima di
  aggiungere voci ([](/design/filesystem/server/#concurrent-appenders)).
- **Un solo `serve` per directory dei blob.** I blocchi di caricamento e la finalizzazione vengono
  rivendicati all'interno del processo che serve le richieste ([](/design/transfer/upload/)),
  quindi due processi `serve` sullo stesso albero, sullo stesso host o su due host, non sono
  supportati. `gc`, `purge` e `scrub` possono essere eseguiti accanto a esso; `rebuild --apply`
  richiede che sia fermo ([](#manutenzione)).

Supportati:

- ext4, XFS e btrfs su storage locale o a blocchi: un disco, RAID, LVM, iSCSI o un volume a blocchi
  nel cloud;
- ZFS con `sync=standard` (il predefinito) o `sync=always`;
- NFSv4, oppure NFSv3 con il suo lock manager, esportato con `sync`;
- un PersistentVolume di Kubernetes o un volume di container basato su una delle opzioni
  precedenti.

Non supportati:

- un mount che disattiva le barriere di scrittura, o un disco virtuale il cui host ignora i flush;
- ZFS con `sync=disabled`, che conferma `fsync` senza scrivere;
- NFS esportato con `async`, che conferma scritture che il server NFS non ha memorizzato, oppure
  montato con `nolock`, `local_lock=flock` o `local_lock=all`, che limitano i lock a un solo host;
- condivisioni SMB/CIFS;
- file system FUSE (sshfs, rclone mount, s3fs, gcsfuse) e qualsiasi mount di un object storage;
- file system di pooling e di unione che distribuiscono un albero su più dischi (mergerfs,
  condivisioni utente di Unraid sotto `/mnt/user`): una rinomina tra dischi membri è una copia, non
  una rinomina atomica;
- il livello scrivibile di un container, e una directory di un host macOS o Windows montata con
  bind mount nella VM dei container. Usa un volume all'interno della VM.

Su Linux, `flock` su NFS viene emulato con lock su intervalli di byte, che il lock manager condivide
tra gli host. Con i lock disattivati, `serve` e `purge --apply` su host diversi possono intercalare
le loro aggiunte a un registro.

- **Esegui una prova di interruzione di corrente prima di fidarti dell'hardware.** All'avvio `serve`
  controlla il tipo di file system, la rinomina atomica e la condivisione dei lock, ma nessun
  controllo software può accorgersi di un disco, un controller o un hypervisor che conferma un flush
  che non ha eseguito. Sull'hardware di produzione, carica dei file mentre togli la corrente (non uno
  spegnimento regolare), poi riavvia ed esegui `capsule-server scrub --deep`: ogni caricamento
  confermato dal server deve essere intatto
  ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Lascia dello spazio libero.** La radice dei blob mantiene una riserva di spazio libero, per
  impostazione predefinita il valore maggiore tra il 2% del volume e 4 GiB. Al di sotto, i nuovi
  caricamenti vengono rifiutati con `507`, mentre quelli già sul server vengono completati e le
  eliminazioni continuano a funzionare ([](/design/quota/#enforcement-points)). La riserva è
  pianificata ma non ancora implementata; fino ad allora, un disco pieno rifiuta ogni scrittura.
- **Ripulisci la quarantena in modo consapevole.** `quarantine/` conserva i blob che non hanno
  superato un nuovo calcolo dell'hash o che non è stato possibile leggere finché non li elimini, a
  eccezione di quelli riparati nel frattempo. Il futuro `capsule-server quarantine prune` mostra
  l'elenco prima di rimuovere e non rimuove nulla senza `--apply`
  ([](/design/filesystem/server/#quarantine-retention)). `scrub` riporta la dimensione della
  quarantena, quindi tienila d'occhio lì.

### Reverse proxy

TLS termina sul proxy, mai sul server ([](/design/networking/#transport-security)). Il server è in
ascolto su HTTP in chiaro e deve essere raggiungibile solo dal proxy: su loopback, su una rete privata
o su una rete di pod. Configura il proxy in modo che:

- accetti corpi delle richieste di almeno 16 MiB, il blocco di caricamento più grande;
- trasmetta in streaming i corpi delle richieste al server invece di bufferizzarli;
- disattivi i dati anticipati di TLS (0-RTT), oppure li inoltri con l'header `Early-Data: 1` così
  che il server possa rifiutare una scrittura;
- non applichi alcun timeout sulla durata totale del corpo di una richiesta, e usi un timeout di
  inattività più lungo della soglia di stallo del server, così un blocco lento ma costante non viene
  mai interrotto;
- **lasci passare la compressione.** Il server decide quali risposte comprimere e non comprime mai
  `application/octet-stream` ([](/design/networking/#compression)). Il proxy inoltra le risposte di
  Capsule così come il server le ha inviate: NON DEVE comprimere una risposta con gzip né in altro
  modo, NON DEVE decomprimerla né aggiungere, modificare o rimuovere `Content-Encoding`, e DEVE
  inoltrare invariato l'`Accept-Encoding` del client. In nginx, mantieni `gzip off;` per il sito di
  Capsule, che sovrascrive un `gzip on;` ereditato dal `nginx.conf` della distribuzione. In Caddy,
  non aggiungere alcuna direttiva `encode`; in Traefik, non associare alcun middleware `compress`.

HTTP/2 e HTTP/3 sul lato client del proxy sono facoltativi; i client ripiegano su HTTP/1.1. TLS nel
kernel (kTLS) e l'offload TLS sulla scheda di rete, se li desideri, sono impostazioni del proxy (in
nginx, `ssl_conf_command Options KTLS;` con `sendfile on;`). Lo stack TLS del server viene usato solo
per le connessioni che apre lui stesso: verso un provider OpenID Connect e verso i peer federati.
Queste si fidano delle radici Mozilla incluse più un eventuale `OIDC_CA_BUNDLE`, non dell'archivio
dei certificati dell'host.

<a id="clock"></a>

### Orologio

Il server misura il tempo con il proprio orologio, e diverse regole dipendono dalla sua esattezza.
Mantienilo sincronizzato con NTP (chrony o systemd-timesyncd) sull'host. Un container legge
l'orologio dell'host, quindi sincronizza l'host. Lo scarto DOVREBBE restare entro un secondo, e DEVE
restare entro 30 secondi. Oltre questi limiti:

| Scarto | Cosa si rompe |
| --- | --- |
| Oltre 30 s | I codici del secondo fattore iniziano a fallire: un codice viene accettato solo entro un intervallo di 30 s rispetto all'orologio del server ([](/design/authentication/)) |
| Oltre 5 min | La federazione si blocca in entrambe le direzioni: i peer rifiutano le richieste, i suggerimenti e i report firmati di questo server, e lui rifiuta i loro ([](/design/federation/)) |
| Orologio avanti | Il cestino viene svuotato prima della fine del periodo di conservazione, e un ripristino fatto in tempo viene rifiutato ([](/design/organization/#retention-window)). Sessioni, link di condivisione e sessioni di caricamento scadono in anticipo |
| Orologio indietro | Sessioni, link di condivisione e contatori scaduti restano validi più a lungo di quanto configurato |
| Scarto grossolano | I client rifiutano le ricevute di custodia il cui `received_at` non è plausibile ([](/design/transfer/durability/)) |

Avvia `serve` solo quando l'orologio è sincronizzato. Con systemd, ordinalo dopo `time-sync.target`
con `chrony-wait.service` o `systemd-time-wait-sync.service` abilitato. Una macchina virtuale
ripristinata da uno snapshot riparte con l'orologio dello snapshot: lascia che NTP lo corregga prima
che `serve` si avvii. Il server non rileva né segnala ancora da solo lo scarto dell'orologio.

## Hardware

Questi valori si riferiscono a un nodo che esegue insieme il server e PostgreSQL. Sono indicazioni di
dimensionamento ricavate da ciò che fa il server, non benchmark.

Il lavoro del server è leggero, quindi la maggior parte del budget per l'hardware dovrebbe andare
allo **storage**. Calcola l'hash SHA-256 di ogni blocco man mano che arriva e lo ricalcola alla
finalizzazione e durante `scrub --deep`. Verifica la firma ibrida del client (Ed25519 e ML-DSA-65)
alla finalizzazione, firma le ricevute allo stesso modo ed esegue Argon2id una volta per ogni accesso
con password (circa 19 MiB ciascuno). Non decodifica, ridimensiona o transcodifica mai i contenuti
multimediali, non cifra né decifra mai, e lascia TLS al reverse proxy. Il corpo di un caricamento non
viene mai bufferizzato oltre un blocco, quindi la memoria non cresce con la dimensione del file.

| | Minimo | Consigliato |
| --- | --- | --- |
| **CPU** | 2 core, x86_64 o arm64 | 4 core. Le istruzioni SHA-2 (SHA-NI su x86_64, le estensioni crittografiche Armv8) velocizzano il calcolo degli hash |
| **RAM** | 2 GB | Da 4 a 8 GB. Di più aiuta la cache di PostgreSQL su una libreria grande, ma lo storage ripaga prima |
| **Storage di sistema** | SSD, 10 GB liberi | SSD o NVMe per PostgreSQL |
| **Storage dei blob** | La dimensione della tua libreria, più spazio per i caricamenti in corso | Dischi ridondanti (mirror, RAID o ZFS) dimensionati per diversi anni di crescita. I dischi meccanici vanno bene |
| **Rete** | 10 Mbit/s in upload | Gigabit sulla LAN, e 50 Mbit/s o più in upload se i dispositivi si sincronizzano dall'esterno |
| **Alimentazione** | — | Un UPS, così un'interruzione di corrente non interrompe PostgreSQL a metà di una scrittura |

- **`BLOB_ROOT` resta su un unico file system.** L'intero albero, `incoming/` compresa, deve essere
  un unico file system, quindi non può essere diviso tra un SSD e un array di dischi né tra due
  dataset ZFS. Metti tutto sull'array.
- **Dai a PostgreSQL la memoria che il server non usa.** Ogni richiesta autenticata legge la propria
  sessione da PostgreSQL e ogni blocco di caricamento vi registra il proprio intervallo, quindi la sua
  cache serve il percorso delle richieste oltre all'indice della libreria. Sessioni, sessioni di
  caricamento e contatori sono piccoli, e il server li elimina appena scadono.
- **La banda in upload è il limite per i dispositivi remoti.** Un telefono fuori dalla tua rete
  carica alla velocità di *download* della tua connessione, ma ogni recupero di un'anteprima o di un
  originale esce alla tua velocità di *upload*. Un blocco viene interrotto solo quando smette di
  arrivare, mai perché ci mette troppo, e un client dimensiona i propri blocchi per completarli in un
  breve tempo obiettivo alla velocità che misura, quindi un collegamento lento carica al proprio
  ritmo. Mantieni la soglia di stallo del server al di sotto del timeout di inattività del proxy,
  così il server risponde a un blocco in stallo prima che il proxy lo interrompa.
- **Dimensiona i dischi per gli originali, non per i derivati.** I derivati sono una piccola frazione
  di una libreria di file RAW e video. Il server conserva ogni blob una sola volta, così come il
  client l'ha cifrato.
- **Gli utenti aggiungono banda, non calcolo.** Ricerca, miniature e organizzazione vengono eseguite
  sui client. Più persone che navigano significano più byte serviti, ma poca CPU o RAM in più.

`capsule-server/compose.yaml` esegue Postgres con le impostazioni giuste per l'uso locale
(`mise run serve-deps`). Le sue credenziali sono solo per lo sviluppo.

<a id="build"></a>

## Compilazione

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

I binari sono collegati a glibc e non richiedono nient'altro di installato. Eseguili come utente non
privilegiato il cui unico percorso scrivibile sia `BLOB_ROOT`, la stessa configurazione imposta
dall'immagine container descritta di seguito.

<a id="container-image"></a>

## Immagine container

:::caution
Non è ancora stata pubblicata alcuna immagine, e le route `/livez` e `/readyz` e il sottocomando
`healthcheck` non sono ancora implementati. Questa sezione è il contratto in base al quale viene
costruita l'immagine.
:::

- **Base:** `gcr.io/distroless/cc-debian12:nonroot`, fissata tramite digest. Contiene glibc, il
  runtime di GCC, i certificati delle CA e i dati dei fusi orari, e nessuna shell né gestore di
  pacchetti.
- **Architetture:** un'unica immagine multi-arch per `linux/amd64` e `linux/arm64`, ciascuna
  compilata nativamente per il proprio target glibc. Le build non sono ottimizzate per una CPU
  specifica; il calcolo degli hash sceglie SHA-NI o le estensioni crittografiche Armv8 in fase di
  esecuzione.
- **Contenuto:** `capsule-server` (l'entrypoint, `serve` per impostazione predefinita) e
  `capsule-server-migration`, dalla stessa build.
- **Utente:** non root, UID e GID 65532. Il container non ha bisogno di alcuna capability Linux:
  rimuovile tutte e imposta `no-new-privileges`.
- **File system radice in sola lettura.** Il server scrive solo sotto `BLOB_ROOT`, quindi monta la
  directory dei blob come unico volume scrivibile, di proprietà dell'UID 65532, e monta la radice in
  sola lettura.
- **Listener:** imposta `SERVER_HOST=0.0.0.0` (o `::`) all'interno del container; la porta 3000 è
  quella predefinita. Pubblicala solo verso il reverse proxy ([](#reverse-proxy)).
- **I log** vanno su stderr; imposta `LOG_FORMAT=json` per un sistema di raccolta dei log.
- **Arresto:** `serve` si arresta in modo ordinato con `SIGTERM`. Un caricamento interrotto da un
  arresto riprende dagli intervalli già confermati dal server.
- **Una sola replica.** Esegui esattamente un `serve` per directory dei blob
  ([](#directory-dei-blob)). Su Kubernetes, usa una replica con la strategia di aggiornamento
  `Recreate`: un aggiornamento progressivo avvia il nuovo server mentre quello vecchio è ancora in
  servizio.
- **Le migrazioni** vengono eseguite dallo stesso tag dell'immagine prima dell'avvio di `serve`,
  come Job, init container o `ExecStartPre=`: `capsule-server-migration up`
  ([](#migra-poi-avvia)).
- **I comandi di manutenzione** vengono eseguiti dalla stessa immagine, con lo stesso utente e sullo
  stesso volume, come CronJob o timer ([](#manutenzione)).
- **Stato di salute:** `/livez` risponde a un `GET` mentre il processo è in servizio e non controlla
  nient'altro, per una liveness probe. `/readyz` risponde solo mentre PostgreSQL risponde, lo schema
  corrisponde al binario e `BLOB_ROOT` è scrivibile, per una readiness probe. Entrambe si trovano
  fuori da `/v1`, quindi una probe non dipende dalla versione del protocollo, e nessuna delle due
  deve essere raggiungibile attraverso il proxy. L'immagine non contiene `curl`, quindi il suo
  health check esegue `capsule-server healthcheck`, che chiama `/livez` sul listener locale.
- **Orologio:** il container legge l'orologio dell'host; sincronizza l'host ([](#orologio)).

<a id="configure"></a>

## Configurazione

La configurazione avviene solo tramite l'ambiente. `capsule-server/.env.example` è un modello di
partenza, e questa pagina indica le variabili su cui chi gestisce il server deve decidere. Un server
durevole si rifiuta di avviarsi (uscita 2), indicando tutte le variabili mancanti in una volta sola,
senza:

| Variabile | Scopo |
| --- | --- |
| `DATABASE_URL` | Connessione a PostgreSQL |
| `BLOB_ROOT` | Directory dei blob |
| `JWT_ED25519_DER` | Chiave Ed25519 in Base64 che firma i token di sessione |
| `ATTESTATION_KEY_SEED` | Seed in Base64 che firma le ricevute di custodia; deve essere indipendente dalla chiave dei token |

- **Proteggi soprattutto la chiave dei token.** Il database non contiene alcun segreto bearer,
  quindi un database o un backup trapelato non fornisce da solo alcuna sessione; `JWT_ED25519_DER` è
  ciò che trasforma una fuga di questo tipo in tutte le sessioni attive
  ([](/design/authentication/#session-token)). Il rilevamento del riutilizzo dei token di refresh
  ([](/design/authentication/#token-lineage-and-reuse)) non aiuta contro di essa, perché chi possiede
  la chiave genera token invece di riutilizzarli. Conservala nel tuo archivio di segreti, mai nel
  database né nei suoi backup, e ruotala dopo una sospetta fuga, il che disconnette tutti i
  dispositivi.
- **È pianificata una chiave di sigillatura per il secondo fattore.** Il server sigillerà a riposo i
  segreti TOTP con una chiave dedicata, indipendente dalla chiave dei token, così una fuga del
  database o di un backup non fornisce alcun secondo fattore e la rotazione della chiave dei token
  non ne rende inutilizzabile nessuno ([](/design/authentication/#the-second-factor)). La relativa
  impostazione non esiste ancora; quando arriverà, tienila separata dal database e dai suoi backup,
  come la chiave dei token.
- **La registrazione aperta richiede una quota predefinita** (pianificato). Quando le quote saranno
  configurabili, `serve` si rifiuterà di avviarsi con la registrazione tramite password attivata e
  nessun limite predefinito, e chi gestisce il server e non vuole alcun limite dovrà impostarlo
  esplicitamente a `unlimited`, cosa che viene registrata a livello `WARN` a ogni avvio
  ([](/design/quota/#thresholds-and-states)). Oggi ogni installazione funziona senza limiti.

`SYNC_CURSOR_MAC_KEY` viene derivata da `JWT_ED25519_DER` quando non è impostata, quindi ruotare la
chiave dei token invalida anche i cursori di sincronizzazione in sospeso, a meno che tu non la
imposti esplicitamente. La federazione è disattivata a meno che non siano impostate
`FEDERATION_ENABLED=true` e `FEDERATION_URL`; lasciala disattivata.

Indica il tuo reverse proxy, così i limiti di frequenza per origine vedono il client e non il proxy.

| Variabile | Significato |
| --- | --- |
| `TRUSTED_PROXIES` | Indirizzi e reti CIDR separati da virgole (`10.0.0.0/8`). Una rete `/0` viene rifiutata, perché crederebbe a qualsiasi mittente. |
| `TRUSTED_PROXY_HOPS` | Considera attendibili altrettanti hop di inoltro più vicini al server, qualunque sia il loro indirizzo. |
| `TRUSTED_PROXY_HEADER` | L'unico campo di inoltro che il server legge: `x-forwarded-for` (il predefinito) o `forwarded` (RFC 7239). Qualsiasi altro valore impedisce l'avvio. |

Se non sono impostate, nessun header di inoltro viene considerato attendibile. È sicuro, ma allora
tutti i client dietro il proxy condividono il budget del proxy, così un estraneo che tenta di
indovinare le password rallenta l'accesso per tutti ([](/design/authentication/#sign-in-limits)); il
server registra un avviso quando vede un header di inoltro senza considerare attendibile alcun hop.
L'errore opposto è peggiore: considerare attendibile un hop che non aggiunge l'indirizzo del client
permette a ogni client di scegliere la propria chiave di limitazione. Il campo che
`TRUSTED_PROXY_HEADER` non indica viene ignorato, chiunque lo invii, quindi un client non può
scegliere la propria chiave di limitazione inviandone uno che il tuo proxy lascia passare. Indica il
campo a cui il tuo proxy aggiunge l'indirizzo. Con il valore predefinito, fai in modo che il tuo
proxy aggiunga a `X-Forwarded-For` l'indirizzo che ha visto, come fa nginx con:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Imposta `TRUSTED_PROXY_HEADER=forwarded` solo se il tuo proxy aggiunge il proprio elemento a
`Forwarded` e non scrive `X-Forwarded-For`.

<a id="migrate-then-start"></a>

## Migra, poi avvia

`serve` non esegue mai le migrazioni e rifiuta uno schema del database più vecchio del binario.
Esegui prima le migrazioni, a ogni aggiornamento:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` scrive i log su stderr e stampa `listening on <url>` su stdout. Non usare `--memory` in
produzione: mantiene tutto, tranne i blob, nella memoria del processo.

<a id="maintenance"></a>

## Manutenzione

Pianifica questi comandi a esecuzione singola (cron o un timer). Richiedono `DATABASE_URL` e
`--blob-root`, e per impostazione predefinita eseguono una simulazione:

- `capsule-server gc --apply`: recupera i blob non referenziati dopo un periodo di tolleranza ed
  elimina sessioni, sessioni di caricamento e contatori scaduti. Eseguilo regolarmente anche quando
  lo spazio non scarseggia.
- `capsule-server purge --apply`: elimina definitivamente gli elementi eliminati in modo logico il
  cui periodo di conservazione è scaduto. Richiede anche l'`ATTESTATION_KEY_SEED` del server,
  perché ogni eliminazione definitiva viene prima firmata nel registro del proprietario
  ([](/design/filesystem/server/#the-owner-ledger)), e senza di esso si rifiuta di procedere.
  Termina con un codice diverso da zero quando il registro non è riuscito a registrare
  un'eliminazione; quell'elemento viene conservato per l'esecuzione successiva.
- `capsule-server scrub [--deep]`: controllo di integrità in sola lettura dell'indice rispetto
  all'archivio dei blob. **Termina con un codice diverso da zero a ogni anomalia rilevata**, quindi
  imposta un avviso su questo ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

Dopo aver ripristinato un backup o perso il database, ricostruisci l'indice dai registri dei
proprietari sotto `BLOB_ROOT` ([](/design/filesystem/server/#rebuild)). Imposta
`ATTESTATION_KEY_SEED` sull'host così che verifichi le firme dei registri.

1. `capsule-server rebuild` confronta l'indice con i registri.
2. Con `serve` fermo, `capsule-server rebuild --apply` ripristina le righe che mancano
   nell'indice. Non sovrascrive mai una riga già presente nell'indice. Riancora un registro che ha
   perso la parte finale, dopodiché il server torna a scrivere per quel proprietario.
3. Se il report indica che un registro è stato riscritto, il server rifiuta le scritture di quel
   proprietario finché non hai indagato ed eseguito
   `capsule-server rebuild --apply --accept-rewrites`.

Un server che ha applicato operazioni del ciclo di vita (eliminazione, ripristino, aggiornamento dei
metadati, derivato, sostituzione) prima di registrare le dimensioni dei relativi blob conserva quei
riferimenti con dimensione 0. Esegui `capsule-server repair-sizes` una volta per elencarli, poi
`capsule-server repair-sizes --apply` per compilare ogni dimensione a partire dall'archivio dei
blob. È sicuro eseguirlo accanto a un `serve` in funzione, e ogni elemento riparato viene inviato di
nuovo ai client. Termina con un codice diverso da zero quando un blob manca o è vuoto, e lascia quel
riferimento com'era.

<a id="backups"></a>

## Backup

Lo stato durevole è PostgreSQL più `BLOB_ROOT` (i blob e `ledger/`); esegui il backup di entrambi,
insieme. Il database contiene anche le sessioni di accesso e le sessioni di caricamento in corso,
quindi un ripristino mantiene gli utenti connessi. Conserva anche le variabili delle chiavi indicate
sopra nel tuo archivio di segreti: non sono nel database, e l'identità di firma e di attestazione del
server dipende da esse.

Esegui prima il backup del database, poi quello dei blob:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` può continuare a funzionare. Un blob si trova su disco e la sua voce è aggiunta al registro
prima che venga confermata la riga dell'indice che lo nomina, quindi una copia dei blob fatta dopo il
dump contiene ogni blob a cui il dump fa riferimento, ed eventualmente scritture che il dump non ha
colto. Non eseguire `gc --apply` né `purge --apply` mentre il backup è in corso: entrambi possono
rimuovere un blob a cui il dump fa ancora riferimento.

La ricostruzione descritta di seguito ripristina gli elementi caricati tra il dump e la copia dei
blob, perché aggiunge ogni riga che manca nell'indice. Non modifica mai una riga che il dump contiene
già, quindi un'eliminazione, un ripristino o una modifica dei metadati fatti in quell'intervallo su
un elemento più vecchio non vengono applicati. Una ricostruzione segnala una modifica di questo tipo
come riga divergente (e termina con un codice diverso da zero) solo quando ha cambiato i blob
dell'elemento; un'eliminazione o un ripristino passano inosservati. Quando un backup deve catturare
ogni modifica, ferma `serve` mentre esegui entrambe le metà. Se un ripristino segnala righe divergenti
e il vecchio host è ancora in funzione, ripeti il backup in quel modo.

<a id="restoring-onto-a-new-host"></a>

### Ripristino su un nuovo host

1. Avvia un PostgreSQL vuoto e imposta gli stessi `JWT_ED25519_DER` e `ATTESTATION_KEY_SEED` del
   vecchio host.
2. Ripristina il database. Il dump contiene lo schema e il relativo registro delle migrazioni,
   quindi non eseguire prima `capsule-server-migration up`:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Ricopia la directory dei blob in `BLOB_ROOT`, su un unico file system.
4. Con `serve` fermo, esegui `capsule-server rebuild` e leggi il report. Elenca gli elementi
   caricati dopo il dump; `capsule-server rebuild --apply` li ripristina con i numeri di sequenza
   che avevano, così i cursori di sincronizzazione dei client restano validi.
5. Esegui `capsule-server scrub --deep`. Non dovrebbe segnalare nulla; termina con un codice
   diverso da zero a ogni anomalia rilevata.
6. Avvia `serve`. Le sessioni tornano con il database, quindi un client connesso al momento del
   dump resta connesso, a meno che la sua sessione non sia scaduta nel frattempo. Prima di mettersi
   in ascolto, `serve` scarta ogni sessione di caricamento incompleta il cui file temporaneo sotto
   `BLOB_ROOT` non è stato ripristinato, ed elimina ogni file temporaneo senza sessione; un client il
   cui caricamento è stato scartato ricomincia quel file da capo. I client riprendono la
   sincronizzazione da dove si erano fermati.

Se il database va perso e non esiste un dump, esegui le migrazioni su un database vuoto ed esegui
`capsule-server rebuild --apply` sulla directory dei blob. Ripristina ogni riga degli elementi dai
registri dei proprietari. Account, directory dei dispositivi pubblicate, appartenenza agli album e
link di condivisione esistono solo nel database, e una ricostruzione non li ripristina.

Prova questa procedura prima di affidarle dati reali, ad esempio verificando con `capsule restore`
che ogni originale torni identico byte per byte. `capsule-e2e/tests/backup_restore_drill.rs` esegue
gli stessi passaggi su container (`mise run test-rust-containers`).
