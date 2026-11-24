---
title: Dépannage
description: Collecter la version et les journaux qu'un rapport exige, et résoudre les problèmes courants d'un serveur Capsule, d'un import ou d'une bibliothèque.
audit: none
---

Commencez ici quand quelque chose ne va pas. Si rien ci-dessous ne règle le problème, [](/support/)
indique où demander, et les diagnostics de la première section sont ce dont un rapport de bogue a
besoin.

<a id="collect-diagnostics"></a>

## Collecter les diagnostics

### Version

Aucune version n'a été publiée, donc le numéro de version ne permet pas de distinguer les builds.
Tout est compilé depuis les sources : indiquez le commit à partir duquel vous avez compilé, avec
`git rev-parse --short HEAD` dans votre copie locale. La CLI n'a pas d'option de version, et
`capsule-server --version` n'affiche que la version de la crate, commune à tous les builds.

<a id="logs"></a>

### Journaux

La CLI et le serveur écrivent leurs journaux sur **stderr**. Stdout ne contient que ce qu'une
commande affiche, et avec `--json` il s'agit d'un unique document JSON : vous pouvez donc capturer
chacun séparément :

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` règle la quantité de journalisation. Un build de release journalise au niveau `info`
  par défaut et un build de débogage au niveau `debug`. Utilisez `RUST_LOG=debug` pour reproduire
  un problème, et `RUST_LOG=trace` uniquement sur demande : il est très volumineux.
- Le format des journaux du serveur est défini par `LOG_FORMAT` : `pretty` par défaut dans les
  builds de débogage, `json` dans les builds de release ([](/development/local-development/#logs)).
- La TUI plein écran (un `capsule` seul dans un terminal) ne peut pas journaliser sur l'écran
  qu'elle dessine : elle écrit donc `tui.log` dans le répertoire de cache de la CLI, que
  `capsule status` affiche sous Cache Directory. Le fichier est écrasé à chaque session, alors
  copiez-le avant de relancer la TUI.
- La sortie `--json` est le compte rendu le plus précis de ce qu'a fait une commande, car elle
  indique le résultat de chaque élément. Joignez-la quand une commande signale des échecs.

**Masquez avant de partager.** Retirez les phrases secrètes, secrets de récupération, jetons,
clés, adresses de serveur et chemins de fichiers personnels. Ne collez jamais une phrase secrète
de bibliothèque ou un secret de récupération nulle part, y compris dans un rapport privé.

<a id="the-server"></a>

## Le serveur

<a id="it-refuses-to-start"></a>

### Il refuse de démarrer

- **Code de sortie 2, avec des noms de variables.** Un serveur durable a besoin de
  `DATABASE_URL`, `BLOB_ROOT`, `JWT_ED25519_DER` et `ATTESTATION_KEY_SEED`, et nomme d'un coup
  chacune de celles qui manquent. Voir [](/guides/self-hosting/#configure).
- **Le schéma de la base de données est plus ancien que le binaire.** `serve` ne migre jamais.
  Exécutez `capsule-server-migration up` à chaque mise à niveau, avant `serve`
  ([](/guides/self-hosting/#migrate-then-start)).
- **Un réglage de proxy de confiance est refusé.** `TRUSTED_PROXIES` refuse un réseau `/0`, et
  `TRUSTED_PROXY_HEADER` n'accepte que `x-forwarded-for` ou `forwarded`.

Un serveur en cours d'exécution affiche `listening on <url>` sur stdout.

<a id="clients-cant-sign-in-or-connect"></a>

### Les clients ne peuvent pas se connecter

- `capsule server-info` montre comment le serveur authentifie les utilisateurs : s'il dispose
  d'un fournisseur d'identité et s'il refuse les mots de passe. Une connexion par mot de passe à
  un serveur où les mots de passe sont désactivés échoue par conception.
- Derrière un proxy inverse, tous les clients partagent la limite de débit du proxy tant que
  celui-ci n'est pas nommé dans `TRUSTED_PROXIES` ou `TRUSTED_PROXY_HOPS`. Quand plusieurs
  personnes sont limitées en même temps, vérifiez cela en premier
  ([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### L'index et le stockage de blobs divergent

`capsule-server scrub` est une vérification d'intégrité en lecture seule, qui se termine avec un
code non nul à la moindre anomalie. Après la restauration d'une sauvegarde ou la perte de la base
de données, `capsule-server rebuild` compare l'index aux registres des propriétaires. Il ne
modifie rien tant que vous n'avez pas arrêté `serve` et lancé `capsule-server rebuild --apply`
([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## Imports et téléversements

- **Un `push` s'est arrêté en cours de route.** Relancez-le. Les téléversements reprennent là où
  ils se sont arrêtés, donc relancer est toujours sans risque. `push` se termine avec un code non
  nul si un élément a échoué, et `--json` liste le résultat de chaque élément
  ([](/guides/user-guide/#import-and-back-up)).
- **Gardez vos originaux tant que la conservation n'est pas vérifiée.** Ne supprimez pas un
  fichier source avant que `push` n'ait signalé son élément comme vérifié, ou utilisez
  `import --move --push`, qui ne supprime chaque source qu'après la vérification de son élément.
- **Un export Google Photos semble incomplet.** Vérifiez que chaque partie de l'export Takeout a
  été téléchargée et extraite, et lisez ce que l'outil d'import ne peut pas garantir
  ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Les dates de prise de vue sont fausses.** `capsule repair capture-time` signale chaque élément
  dont la date de prise de vue signée ne concorde pas avec l'EXIF de l'original, et `--apply` les
  corrige. Chaque correction est un enregistrement signé permanent, alors lancez-la d'abord sans
  `--apply`.

<a id="check-a-librarys-integrity"></a>

## Vérifier l'intégrité d'une bibliothèque

Si des photos semblent manquantes ou endommagées, **arrêtez-vous et ne supprimez rien** pendant
que vous cherchez la cause : une suppression peut faire disparaître le contexte nécessaire à la
récupération.

1. `capsule verify --library <path>` demande au serveur s'il conserve durablement chaque original.
   `--deep` fait recalculer au serveur le hachage des octets stockés au lieu de vérifier seulement
   leur présence, et `--signed` garde dans la bibliothèque le verdict signé du serveur.
2. Les éléments mis à la corbeille restent récupérables pendant leur délai de conservation :
   `capsule trash restore --library <path> --asset <id>` en restaure un
   ([](/guides/user-guide/#delete-safely)).
3. La perte d'un appareil ou d'une clé a ses propres procédures :
   [](/design/backup-recovery/#recovery-paths).

Si des données sont perdues ou corrompues, signalez-le comme un bogue avec la sortie de chaque
étape. Perdre ou corrompre des données est un défaut, quelle qu'en soit la cause.

<a id="still-stuck"></a>

## Toujours bloqué

[](/support/) indique où poser une question et comment rédiger un rapport de bogue sur lequel on
peut agir.
