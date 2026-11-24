---
title: Guida per l'utente
description: L'uso quotidiano di una libreria Capsule con la CLI capsule — importare, fare il backup, verificare, organizzare ed eliminare in sicurezza.
audit: none
---

Questa guida tratta la CLI `capsule`, il client di riferimento. I comandi contrassegnati come
_(pianificato)_ sono nella [roadmap](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)
e non ancora disponibili. `capsule <command> --help` è il riferimento autorevole per le opzioni; il
riferimento della CLI ([](/reference/cli/)) le elenca tutte.

:::caution
Capsule è in fase alpha. Fino alla prima release per l'uso interno, conserva un backup indipendente
di ogni originale che importi.
:::

<a id="set-up"></a>

## Configurazione

1. Crea una libreria: `capsule library init --name "Photos"`. La libreria è una directory locale
   cifrata; la sua passphrase protegge le chiavi su questo computer.
2. Accedi al tuo server: `capsule auth register` oppure `capsule auth login` (`--oidc` o
   `--device` per il single sign-on).
3. Configura il recupero: `capsule auth escrow`. Conserva offline il segreto di recupero stampato.
   Senza di esso e senza la tua passphrase, perdere il computer significa perdere la libreria,
   perché il server non può decifrare nulla.

<a id="import-and-back-up"></a>

## Importare e fare il backup

- `capsule import <path>` copia i file nella libreria, firma la provenienza e i metadati di ogni
  elemento e genera le miniature. `--provider takeout` importa un archivio di Google Takeout con i
  relativi metadati sidecar ([](/guides/google-photos-migration/)).
- `capsule push` carica la libreria. I caricamenti riprendono da dove si erano interrotti, quindi
  eseguire di nuovo `push` è sempre sicuro. Dopo ogni elemento, `push` chiede al server di
  confermare che conserva in modo durevole ogni blob e salva nella libreria le ricevute di custodia
  firmate dal server. Un elemento non riuscito viene segnalato e l'esecuzione prosegue. Il comando
  termina con un codice diverso da zero se un elemento non è riuscito, e `--json` elenca l'esito di
  ciascuno.
- `capsule import --push` fa entrambe le cose in un solo passaggio.
- **Conserva i tuoi originali finché la custodia non è verificata.** Un elemento è al sicuro solo
  quando il server lo dichiara durevole e ha emesso una ricevuta di custodia firmata, che `push`
  segnala come verificata. Non eliminare i file di origine prima di allora. `import --move --push`
  elimina ogni file di origine solo dopo che il suo elemento è stato verificato. Senza `--push`,
  `--move` conserva tutti i file di origine e lo segnala, e un successivo `import --move --push`
  sugli stessi file li rilascia.
- `capsule restore` _(pianificato)_ scarica gli originali e li confronta byte per byte con quanto
  firmato all'importazione.

<a id="organize"></a>

## Organizzare

- Le **etichette** sono libere e si applicano a un elemento, a uno stack (coppia RAW+JPEG, raffica,
  bracketing), a un album o a una regione di un'immagine. Le etichette che aggiungi restano separate
  da quelle suggerite dall'IA, e puoi accettare o scartare un suggerimento. Le etichette degli
  elementi esistono già; quelle di stack, album e regioni sono _(pianificate)_.
- `capsule show <asset>` mostra ciò che registrano i metadati firmati di un elemento: didascalia,
  valutazione, etichette, posizione, data e ora di scatto.
- `capsule repair capture-time` trova gli elementi la cui data di scatto non corrisponde all'EXIF
  dell'originale e, con `--apply`, la corregge. La modifica della posizione è _(pianificata)_.

Ogni modifica è un cambiamento firmato e di sola aggiunta: il valore precedente resta nella
cronologia di provenienza dell'elemento, e le modifiche simultanee da due dispositivi vengono unite
in modo deterministico.

<a id="delete-safely"></a>

## Eliminare in sicurezza

Correggi metadati come la geolocalizzazione prima di eliminare, finché gli elementi vicini ti danno
ancora un contesto. Una foto senza una propria posizione è facile da collocare finché le foto vicine
della stessa passeggiata sono nella libreria; quando spariscono, sparisce anche quel contesto.

- `capsule cull` contrassegna gli elementi come scelto, neutro o scartato e filtra per
  contrassegno.
- `capsule cull --sweep` sposta gli scartati nel cestino. Gli elementi nel cestino restano
  recuperabili per il periodo di conservazione (`--retain-days`) prima di essere eliminati
  definitivamente.
- `capsule trash delete --asset <id>` sposta un elemento nel cestino sia sul server sia nella
  libreria. Un elemento già nel cestino, ad esempio dopo `capsule cull --sweep`, mantiene il periodo
  di conservazione con cui è stato cestinato: `--retain-days` vale solo per una nuova eliminazione,
  e `--json` riporta il periodo inviato come `retention_until`.
- `capsule trash restore --asset <id>` ripristina un elemento. Un ripristino dopo la chiusura del
  periodo di conservazione viene rifiutato (`retention_elapsed` con `--json`) e non viene ritentato;
  l'elemento resta nel cestino finché non viene eliminato definitivamente.

<a id="share"></a>

## Condividere

- `capsule share create --album <id>` oppure `--asset <id>` crea un link di sola visualizzazione;
  `--protect` aggiunge una passphrase al link e `--expires-at` una scadenza. Il link porta la sua
  chiave nel frammento dell'URL, che non raggiunge mai il server.
- `capsule share revoke <id>` impedisce che un link si apra.

<a id="sync-another-machine"></a>

## Sincronizzare un altro computer

- `capsule sync` scarica dal server il feed delle modifiche della libreria; `capsule list` mostra
  ciò che ha ricevuto.
- `capsule auth recover` porta l'account su un nuovo computer a partire dal segreto di recupero.
