---
title: Risoluzione dei problemi
description: Raccogli la versione e i log necessari a una segnalazione, e risolvi i problemi più comuni con un server Capsule, un'importazione o una libreria.
audit: none
---

Inizia da qui quando qualcosa va storto. Se niente di quanto segue risolve il problema, [](/support/)
indica dove chiedere, e la diagnostica della prima sezione è ciò che serve a una segnalazione di bug.

<a id="collect-diagnostics"></a>

## Raccogliere la diagnostica

<a id="version"></a>

### Versione

Non è stata pubblicata alcuna release, quindi il numero di versione non distingue una build
dall'altra. Tutto viene compilato dal codice sorgente, quindi indica il commit da cui hai compilato:
`git rev-parse --short HEAD` nella tua copia del repository. La CLI non ha un'opzione per la
versione, e `capsule-server --version` stampa solo la versione del crate, che è la stessa per ogni
build.

<a id="logs"></a>

### Log

La CLI e il server scrivono i log su **stderr**. Stdout contiene solo ciò che un comando stampa, e
con `--json` si tratta di un unico documento JSON, quindi puoi catturarli separatamente:

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` stabilisce quanto viene registrato. Una build di release registra a livello `info` per
  impostazione predefinita e una build di debug a livello `debug`. Usa `RUST_LOG=debug` quando
  riproduci un problema, e `RUST_LOG=trace` solo se richiesto: è molto voluminoso.
- Il formato dei log del server si imposta con `LOG_FORMAT`: `pretty` per impostazione predefinita
  nelle build di debug, `json` nelle build di release ([](/development/local-development/#logs)).
- La TUI a schermo intero (un semplice `capsule` in un terminale) non può scrivere i log sullo
  schermo che disegna, quindi scrive `tui.log` nella directory della cache della CLI, che
  `capsule status` mostra come Cache Directory. Il file viene sovrascritto a ogni sessione, quindi
  copialo prima di avviare di nuovo la TUI.
- L'output di `--json` è la registrazione più precisa di ciò che ha fatto un comando, perché riporta
  l'esito di ogni elemento. Allegalo quando un comando segnala errori.

**Oscura i dati sensibili prima di condividere.** Rimuovi passphrase, segreti di recupero, token,
chiavi, indirizzi del server e percorsi di file personali. Non incollare mai la passphrase di una
libreria o un segreto di recupero da nessuna parte, nemmeno in una segnalazione privata.

<a id="the-server"></a>

## Il server

<a id="it-refuses-to-start"></a>

### Non si avvia

- **Stato di uscita 2, con i nomi delle variabili.** Un server durevole richiede `DATABASE_URL`,
  `BLOB_ROOT`, `JWT_ED25519_DER` e `ATTESTATION_KEY_SEED`, e indica tutte quelle mancanti in una
  volta sola. Vedi [](/guides/self-hosting/#configure).
- **Lo schema del database è più vecchio del binario.** `serve` non esegue mai le migrazioni.
  Esegui `capsule-server-migration up` a ogni aggiornamento, prima di `serve`
  ([](/guides/self-hosting/#migrate-then-start)).
- **Un'impostazione dei proxy attendibili viene rifiutata.** `TRUSTED_PROXIES` rifiuta una rete
  `/0`, e `TRUSTED_PROXY_HEADER` accetta solo `x-forwarded-for` o `forwarded`.

Un server in esecuzione stampa `listening on <url>` su stdout.

<a id="clients-cant-sign-in-or-connect"></a>

### I client non riescono ad accedere o a connettersi

- `capsule server-info` mostra come il server fa accedere le persone: se ha un provider di identità
  e se rifiuta le password. Un accesso con password a un server con le password disattivate fallisce
  per scelta progettuale.
- Dietro un reverse proxy, tutti i client condividono il limite di frequenza del proxy finché il
  proxy non viene indicato in `TRUSTED_PROXIES` o `TRUSTED_PROXY_HOPS`. Quando più persone
  raggiungono il limite nello stesso momento, controlla prima questo
  ([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### L'indice e l'archivio dei blob non coincidono

`capsule-server scrub` è un controllo di integrità in sola lettura, e termina con un codice diverso
da zero a ogni anomalia rilevata. Dopo aver ripristinato un backup o perso il database,
`capsule-server rebuild` confronta l'indice con i registri dei proprietari. Non modifica nulla
finché non fermi `serve` ed esegui `capsule-server rebuild --apply`
([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## Importazioni e caricamenti

- **Un `push` si è interrotto a metà.** Eseguilo di nuovo. I caricamenti riprendono da dove si
  erano interrotti, quindi rieseguirlo è sempre sicuro. `push` termina con un codice diverso da zero
  quando un elemento non è riuscito, e `--json` elenca l'esito di ciascuno
  ([](/guides/user-guide/#import-and-back-up)).
- **Conserva i tuoi originali finché la custodia non è verificata.** Non eliminare un file di
  origine finché `push` non segnala il suo elemento come verificato, oppure usa
  `import --move --push`, che elimina ogni file di origine solo dopo che il suo elemento è stato
  verificato.
- **Un'esportazione di Google Foto sembra incompleta.** Controlla che ogni parte dell'esportazione
  di Takeout sia stata scaricata ed estratta, e leggi ciò che lo strumento di importazione non può
  garantire ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Le date di scatto sono sbagliate.** `capsule repair capture-time` segnala ogni elemento la cui
  data di scatto firmata non corrisponde all'EXIF dell'originale, e `--apply` le corregge. Ogni
  correzione è un record firmato permanente, quindi eseguilo prima senza `--apply`.

<a id="check-a-librarys-integrity"></a>

## Verificare l'integrità di una libreria

Se alcune foto sembrano mancanti o danneggiate, **fermati e non eliminare nulla** mentre indaghi:
eliminare può cancellare il contesto che ti serve per recuperarle.

1. `capsule verify --library <path>` chiede al server se conserva in modo durevole ogni originale.
   `--deep` fa ricalcolare al server l'hash dei byte memorizzati invece di controllare solo che siano
   presenti, e `--signed` salva nella libreria il verdetto firmato del server.
2. Gli elementi nel cestino restano recuperabili per il loro periodo di conservazione:
   `capsule trash restore --library <path> --asset <id>` ne ripristina uno
   ([](/guides/user-guide/#delete-safely)).
3. La perdita di un dispositivo o di una chiave ha procedure proprie:
   [](/design/backup-recovery/#recovery-paths).

Se dei dati sono andati persi o danneggiati, segnalalo come bug allegando l'output di ogni
passaggio. Perdere o danneggiare dati è un difetto, qualunque ne sia la causa.

<a id="still-stuck"></a>

## Ancora bloccato?

[](/support/) indica dove fare una domanda e come aprire una segnalazione di bug su cui si possa
intervenire.
