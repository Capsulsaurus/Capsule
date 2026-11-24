---
title: Auto-hébergement
description: Faire tourner capsule-server sur PostgreSQL et un stockage de blobs sur système de fichiers
audit: none
---

Aucune version n'a encore été publiée : vous compilez donc le serveur depuis les sources. Le
serveur ne stocke que du texte chiffré et ne décode jamais les médias ; les clients se chargent de
tout le traitement et du chiffrement.

<a id="what-you-need"></a>

## Ce qu'il vous faut

- **PostgreSQL 18** : l'index durable (comptes, éléments, quotas) et l'état éphémère du serveur
  (sessions de connexion, sessions de téléversement, compteurs de débit). C'est la seule base de
  données qu'utilise le serveur, et elle doit tourner avec ses réglages de durabilité intacts
  ([](#postgresql)).
- **Un répertoire de blobs** (`BLOB_ROOT`) sur **un seul système de fichiers POSIX** qui respecte
  `fsync` et partage les verrous de fichiers. Il contient les blobs et, sous `ledger/`, le registre
  signé de chaque propriétaire, à partir duquel l'index peut être reconstruit ([](#répertoire-de-blobs)).
- **Un proxy inverse qui termine le TLS** devant le serveur et laisse passer la compression telle
  quelle ([](#proxy-inverse)).
- **Une horloge synchronisée** sur l'hôte, maintenue par NTP ([](#horloge)).

### PostgreSQL

Le serveur répond à une écriture dès que sa ligne est validée : un commit que PostgreSQL acquitte
puis perd peut donc faire revenir une session révoquée ou faire accepter deux fois un code de
second facteur. Conservez :

- `fsync = on` et `full_page_writes = on`, les valeurs par défaut ;
- `synchronous_commit` à `on`, `local`, `remote_write` ou `remote_apply`, jamais à `off` ;
- le répertoire de données sur un stockage qui respecte les vidages (flush). Un cache d'écriture
  disque sans protection contre les coupures de courant, ou un disque virtuel dont l'hôte ignore
  les vidages, perd des commits acquittés lors d'une coupure ([](/design/filesystem/server/)).

PostgreSQL peut résider sur un autre système de fichiers, un autre disque ou un autre hôte que
`BLOB_ROOT`.

<a id="blob-directory"></a>

### Répertoire de blobs

`BLOB_ROOT` est un contrat avec le système de fichiers
([](/design/filesystem/server/#blob-root-filesystem-contract)), organisé comme dans
[](/design/filesystem/server/#blob-store-layout) :

- **Un seul système de fichiers.** La finalisation est un renommage atomique : l'arborescence
  entière, `incoming/` compris, ne peut donc pas s'étendre sur plusieurs volumes, datasets ou
  PersistentVolumes Kubernetes.
- **`fsync` est respecté**, sur les fichiers comme sur les répertoires, avant que le serveur
  n'acquitte une écriture.
- **Les verrous de fichiers sont partagés** par tous les processus qui écrivent dans
  l'arborescence. `serve` et `purge --apply` verrouillent le `ledger/<owner>/.lock` de chaque
  propriétaire avant d'y ajouter des entrées
  ([](/design/filesystem/server/#concurrent-appenders)).
- **Un seul `serve` par répertoire de blobs.** Les morceaux de téléversement et la finalisation
  sont réservés au sein du processus qui sert ([](/design/transfer/upload/)) : deux processus
  `serve` sur une même arborescence, sur un même hôte ou sur deux, ne sont donc pas pris en
  charge. `gc`, `purge` et `scrub` peuvent tourner à côté ; `rebuild --apply` exige qu'il soit
  arrêté ([](#maintenance)).

Pris en charge :

- ext4, XFS et btrfs sur un stockage local ou en mode bloc : un disque, un RAID, LVM, iSCSI ou un
  volume bloc dans le cloud ;
- ZFS avec `sync=standard` (la valeur par défaut) ou `sync=always` ;
- NFSv4, ou NFSv3 avec son gestionnaire de verrous, exporté en `sync` ;
- un PersistentVolume Kubernetes ou un volume de conteneur reposant sur l'un des éléments
  ci-dessus.

Non pris en charge :

- un montage qui désactive les barrières d'écriture, ou un disque virtuel dont l'hôte ignore les
  vidages ;
- ZFS avec `sync=disabled`, qui acquitte `fsync` sans écrire ;
- NFS exporté en `async`, qui acquitte des écritures que le serveur NFS n'a pas stockées, ou monté
  avec `nolock`, `local_lock=flock` ou `local_lock=all`, qui cantonnent les verrous à un seul
  hôte ;
- les partages SMB/CIFS ;
- les systèmes de fichiers FUSE (sshfs, rclone mount, s3fs, gcsfuse) et tout montage de stockage
  objet ;
- les systèmes de fichiers de regroupement et d'union qui répartissent une arborescence sur
  plusieurs disques (mergerfs, partages utilisateur Unraid sous `/mnt/user`) : un renommage
  entre disques membres est une copie, pas un renommage atomique ;
- la couche inscriptible propre à un conteneur, ainsi qu'un répertoire d'hôte macOS ou Windows
  monté par liaison (bind mount) dans la VM des conteneurs. Utilisez un volume à l'intérieur de
  la VM.

Sous Linux, `flock` sur NFS est émulé par des verrous de plages d'octets, que le gestionnaire de
verrous partage entre les hôtes. Si le verrouillage est désactivé, `serve` et `purge --apply` sur
des hôtes différents peuvent entremêler leurs ajouts à un registre.

- **Faites un exercice de coupure de courant avant de faire confiance au matériel.** `serve`
  vérifie au démarrage le type de système de fichiers, le renommage atomique et le partage des
  verrous, mais aucun contrôle logiciel ne peut détecter un disque, un contrôleur ou un
  hyperviseur qui acquitte un vidage qu'il n'a pas effectué. Sur le matériel déployé, coupez le
  courant pendant un téléversement (pas un arrêt propre), puis redémarrez et lancez
  `capsule-server scrub --deep` : chaque téléversement acquitté par le serveur doit être intact
  ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Laissez de l'espace libre.** La racine des blobs garde une réserve d'espace libre, par défaut
  la plus grande des deux valeurs entre 2 % du volume et 4 Gio. En dessous, les nouveaux
  téléversements sont refusés avec `507` tandis que ceux déjà en cours sur le serveur se terminent
  et que les suppressions fonctionnent toujours ([](/design/quota/#enforcement-points)). La
  réserve est prévue mais pas encore implémentée ; d'ici là, un disque plein refuse toute
  écriture.
- **Purgez la quarantaine à dessein.** `quarantine/` conserve les blobs dont le hachage a échoué
  ou qui n'ont pas pu être lus, jusqu'à ce que vous les supprimiez, sauf ceux réparés depuis. La
  commande prévue `capsule-server quarantine prune` liste avant de supprimer et ne supprime rien
  sans `--apply` ([](/design/filesystem/server/#quarantine-retention)). `scrub` indique la taille
  de la quarantaine : surveillez-la là.

<a id="reverse-proxy"></a>

### Proxy inverse

Le TLS se termine au proxy, jamais au serveur ([](/design/networking/#transport-security)). Le
serveur écoute en HTTP simple et ne doit être joignable que depuis le proxy : en boucle locale, sur
un réseau privé ou sur un réseau de pods. Configurez le proxy pour :

- accepter des corps de requête d'au moins 16 Mio, la taille du plus grand morceau de
  téléversement ;
- transmettre les corps de requête au serveur en flux plutôt que de les mettre en tampon ;
- désactiver les données précoces TLS (0-RTT), ou les transmettre avec l'en-tête `Early-Data: 1`
  pour que le serveur puisse refuser une écriture ;
- n'appliquer aucun délai d'expiration global au corps d'une requête, et un délai d'inactivité
  plus long que le seuil de blocage du serveur, afin qu'un morceau lent mais régulier ne soit
  jamais coupé ;
- **laisser passer la compression.** Le serveur décide quelles réponses compresser et ne
  compresse jamais `application/octet-stream` ([](/design/networking/#compression)). Le proxy
  transmet les réponses amont de Capsule telles que le serveur les a envoyées : il NE DOIT PAS
  (MUST NOT) compresser une réponse avec gzip ou autrement, NE DOIT PAS en décompresser une ni
  ajouter, modifier ou retirer `Content-Encoding`, et DOIT (MUST) transmettre l'`Accept-Encoding`
  du client sans le modifier. Avec nginx, gardez `gzip off;` pour le site Capsule, ce qui
  remplace un `gzip on;` hérité du `nginx.conf` de la distribution. Avec Caddy, n'ajoutez aucune
  directive `encode` ; avec Traefik, n'attachez aucun middleware `compress`.

HTTP/2 et HTTP/3 côté client du proxy sont facultatifs ; les clients se rabattent sur HTTP/1.1.
Le TLS noyau (kTLS) et le déchargement TLS sur la carte réseau, si vous les souhaitez, sont des
réglages du proxy (avec nginx, `ssl_conf_command Options KTLS;` avec `sendfile on;`). La pile TLS
propre au serveur ne sert qu'aux connexions qu'il ouvre lui-même : vers un fournisseur OpenID
Connect et vers les pairs fédérés. Celles-ci font confiance aux racines Mozilla intégrées ainsi
qu'à un éventuel `OIDC_CA_BUNDLE`, et non au magasin de certificats de l'hôte.

<a id="clock"></a>

### Horloge

Le serveur juge le temps d'après sa propre horloge, et plusieurs règles dépendent de son
exactitude. Gardez-la synchronisée par NTP (chrony ou systemd-timesyncd) sur l'hôte. Un conteneur
lit l'horloge de l'hôte : synchronisez donc l'hôte. Le décalage DEVRAIT (SHOULD) rester sous une
seconde, et DOIT (MUST) rester sous 30 secondes. Au-delà :

| Décalage          | Ce qui casse                                                                                                                                                                                                                                 |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Plus de 30 s      | Les codes de second facteur commencent à échouer : un code n'est accepté qu'à un pas de 30 s près de l'horloge du serveur ([](/design/authentication/))                                                                                      |
| Plus de 5 min     | La fédération s'arrête dans les deux sens : les pairs refusent les requêtes, indications et rapports signés de ce serveur, et il refuse les leurs ([](/design/federation/))                                                                  |
| Horloge en avance | La corbeille est purgée avant la fin de son délai de conservation, et une restauration faite à temps est refusée ([](/design/organization/#retention-window)). Les sessions, liens de partage et sessions de téléversement expirent trop tôt |
| Horloge en retard | Les sessions, liens de partage et compteurs expirés restent valides plus longtemps que configuré                                                                                                                                             |
| Décalage grossier | Les clients refusent les reçus de conservation dont le `received_at` est invraisemblable ([](/design/transfer/durability/))                                                                                                                  |

Ne démarrez `serve` qu'une fois l'horloge synchronisée. Sous systemd, ordonnez-le après
`time-sync.target` avec `chrony-wait.service` ou `systemd-time-wait-sync.service` activé. Une
machine virtuelle restaurée depuis un instantané reprend avec l'horloge de l'instantané : laissez
NTP la corriger avant le démarrage de `serve`. Le serveur ne détecte ni ne signale encore lui-même
les dérives d'horloge.

<a id="hardware"></a>

## Matériel

Ces chiffres valent pour un nœud unique faisant tourner ensemble le serveur et PostgreSQL. Ce sont
des recommandations de dimensionnement déduites de ce que fait le serveur, pas des mesures de
performance.

Le travail du serveur est léger : l'essentiel du budget matériel doit aller au **stockage**. Il
calcule le hachage SHA-256 de chaque morceau à son arrivée et le recalcule à la finalisation et
pendant `scrub --deep`. Il vérifie la signature hybride du client (Ed25519 et ML-DSA-65) à la
finalisation, signe les reçus de la même manière, et exécute Argon2id une fois par connexion par
mot de passe (environ 19 Mio à chaque fois). Il ne décode, ne redimensionne et ne transcode jamais
de médias, ne chiffre ni ne déchiffre jamais rien, et laisse le TLS au proxy inverse. Le corps d'un
téléversement n'est jamais mis en tampon au-delà d'un morceau : la mémoire n'augmente donc pas
avec la taille des fichiers.

|                        | Minimum                                                                            | Recommandé                                                                                                                             |
| ---------------------- | ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| **CPU**                | 2 cœurs, x86_64 ou arm64                                                           | 4 cœurs. Les instructions SHA-2 (SHA-NI sur x86_64, les extensions cryptographiques Armv8) accélèrent le hachage                       |
| **RAM**                | 2 Go                                                                               | 4 à 8 Go. Davantage profite au cache de PostgreSQL sur une grande bibliothèque, mais le stockage rapporte en premier                   |
| **Stockage système**   | SSD, 10 Go libres                                                                  | SSD ou NVMe pour PostgreSQL                                                                                                            |
| **Stockage des blobs** | La taille de votre bibliothèque, plus de la place pour les téléversements en cours | Des disques redondants (miroir, RAID ou ZFS) dimensionnés pour plusieurs années de croissance. Des disques durs à plateaux conviennent |
| **Réseau**             | 10 Mbit/s en envoi                                                                 | Gigabit sur le réseau local, et 50 Mbit/s ou plus en envoi si des appareils se synchronisent depuis l'extérieur                        |
| **Alimentation**       | —                                                                                  | Un onduleur, pour qu'une coupure de courant n'interrompe pas PostgreSQL en pleine écriture                                             |

- **`BLOB_ROOT` reste sur un seul système de fichiers.** L'arborescence entière, `incoming/`
  compris, doit constituer un seul système de fichiers : elle ne peut donc pas être répartie entre
  un SSD et une baie de disques, ni entre deux datasets ZFS. Mettez tout sur la baie.
- **Donnez à PostgreSQL la mémoire que le serveur n'utilise pas.** Chaque requête authentifiée lit
  sa session dans PostgreSQL et chaque morceau de téléversement y enregistre sa plage : son cache
  sert donc le chemin des requêtes autant que l'index de la bibliothèque. Les sessions, sessions de
  téléversement et compteurs sont petits, et le serveur les supprime une fois expirés.
- **La bande passante montante est la limite pour les appareils distants.** Un téléphone hors de
  votre réseau téléverse à la vitesse de _réception_ de votre connexion, mais chaque récupération
  d'un aperçu ou d'un original part à votre vitesse d'_envoi_. Un morceau n'est coupé que lorsqu'il
  cesse d'arriver, jamais parce qu'il prend du temps, et un client dimensionne ses morceaux pour
  qu'ils se terminent en un court délai cible à la vitesse qu'il mesure : une liaison lente
  téléverse donc à son propre rythme. Gardez le seuil de blocage du serveur sous le délai
  d'inactivité de votre proxy, afin que le serveur réponde à un morceau bloqué avant que le proxy
  ne le coupe.
- **Dimensionnez les disques pour les originaux, pas pour les dérivés.** Les dérivés ne
  représentent qu'une petite fraction d'une bibliothèque de fichiers RAW et de vidéos. Le serveur
  conserve chaque blob une seule fois, tel que le client l'a chiffré.
- **Les utilisateurs ajoutent de la bande passante, pas du calcul.** La recherche, les miniatures
  et l'organisation s'exécutent sur les clients. Plus de personnes qui naviguent signifie plus
  d'octets servis, mais à peine plus de CPU ou de RAM.

`capsule-server/compose.yaml` lance Postgres avec les bons réglages pour un usage local
(`mise run serve-deps`). Ses identifiants sont réservés au développement.

<a id="build"></a>

## Compilation

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

Les binaires sont liés à la glibc et n'ont besoin de rien d'autre d'installé. Exécutez-les sous un
utilisateur non privilégié dont le seul chemin accessible en écriture est `BLOB_ROOT`, la même
configuration que celle qu'impose l'image de conteneur ci-dessous.

<a id="container-image"></a>

## Image de conteneur

:::caution
Aucune image n'est encore publiée, et les routes `/livez` et `/readyz` ainsi que la sous-commande
`healthcheck` ne sont pas encore implémentées. Cette section est le contrat auquel l'image doit se
conformer.
:::

- **Base :** `gcr.io/distroless/cc-debian12:nonroot`, épinglée par condensé (digest). Elle
  contient la glibc, le runtime GCC, les certificats d'autorité et les données de fuseaux
  horaires, sans shell ni gestionnaire de paquets.
- **Architectures :** une seule image multi-architecture pour `linux/amd64` et `linux/arm64`,
  chacune compilée nativement pour sa cible glibc. Les builds ne sont pas optimisés pour un CPU
  précis ; le hachage choisit SHA-NI ou les extensions cryptographiques Armv8 à l'exécution.
- **Contenu :** `capsule-server` (le point d'entrée, `serve` par défaut) et
  `capsule-server-migration`, issus du même build.
- **Utilisateur :** non root, UID et GID 65532. Le conteneur n'a besoin d'aucune capability
  Linux : retirez-les toutes et activez `no-new-privileges`.
- **Système de fichiers racine en lecture seule.** Le serveur n'écrit que sous `BLOB_ROOT` : montez
  le répertoire de blobs comme unique volume inscriptible, appartenant à l'UID 65532, et montez la
  racine en lecture seule.
- **Écoute :** définissez `SERVER_HOST=0.0.0.0` (ou `::`) dans le conteneur ; le port 3000 est
  celui par défaut. Ne l'exposez qu'au proxy inverse ([](#proxy-inverse)).
- **Les journaux** vont sur stderr ; définissez `LOG_FORMAT=json` pour un collecteur de journaux.
- **Arrêt :** `serve` s'arrête proprement sur `SIGTERM`. Un téléversement interrompu par un arrêt
  reprend à partir des plages validées par le serveur.
- **Un seul réplica.** Exécutez exactement un `serve` par répertoire de blobs
  ([](#répertoire-de-blobs)). Sur Kubernetes, utilisez un seul réplica avec la stratégie de mise à jour
  `Recreate` : une mise à jour progressive démarre le nouveau serveur alors que l'ancien sert
  encore.
- **Les migrations** s'exécutent depuis la même étiquette d'image avant le démarrage de `serve`,
  sous forme de Job, de conteneur d'initialisation ou d'`ExecStartPre=` :
  `capsule-server-migration up` ([](#migrer-puis-démarrer)).
- **Les commandes de maintenance** s'exécutent depuis la même image, sous le même utilisateur, sur
  le même volume, sous forme de CronJobs ou de minuteurs ([](#maintenance)).
- **Santé :** `/livez` répond à un `GET` tant que le processus sert et ne vérifie rien d'autre,
  pour une sonde de vivacité (liveness). `/readyz` ne répond que tant que PostgreSQL répond, que
  le schéma correspond au binaire et que `BLOB_ROOT` est accessible en écriture, pour une sonde de
  disponibilité (readiness). Les deux se trouvent hors de `/v1`, si bien qu'une sonde ne dépend pas
  de la version du protocole, et aucune n'a besoin d'être joignable à travers le proxy. L'image ne
  contient pas `curl` : son propre contrôle de santé exécute donc `capsule-server healthcheck`, qui
  appelle `/livez` sur l'écoute locale.
- **Horloge :** le conteneur lit l'horloge de l'hôte ; synchronisez l'hôte ([](#horloge)).

<a id="configure"></a>

## Configuration

La configuration passe uniquement par l'environnement. `capsule-server/.env.example` est un modèle
de départ, et cette page nomme les variables qu'un opérateur doit choisir. Un serveur durable
refuse de démarrer (code de sortie 2), en nommant d'un coup chaque variable manquante, sans :

| Variable               | Rôle                                                                                               |
| ---------------------- | -------------------------------------------------------------------------------------------------- |
| `DATABASE_URL`         | Connexion à PostgreSQL                                                                             |
| `BLOB_ROOT`            | Répertoire de blobs                                                                                |
| `JWT_ED25519_DER`      | Clé Ed25519 en Base64 qui signe les jetons de session                                              |
| `ATTESTATION_KEY_SEED` | Graine en Base64 qui signe les reçus de conservation ; doit être indépendante de la clé des jetons |

- **Protégez avant tout la clé des jetons.** La base de données ne contient aucun secret porteur :
  une fuite de la base ou d'une sauvegarde ne donne à elle seule aucune session ; c'est
  `JWT_ED25519_DER` qui transforme une telle fuite en accès à toutes les sessions actives
  ([](/design/authentication/#session-token)). La détection de réutilisation des jetons de
  rafraîchissement ([](/design/authentication/#token-lineage-and-reuse)) n'aide pas contre cela,
  car le détenteur de la clé fabrique des jetons au lieu de les rejouer. Conservez-la dans votre
  coffre à secrets, jamais dans la base de données ni dans ses sauvegardes, et renouvelez-la après
  une fuite présumée, ce qui déconnecte tous les appareils.
- **Une clé de scellement du second facteur est prévue.** Le serveur scellera les secrets TOTP au
  repos sous une clé dédiée, indépendante de la clé des jetons, afin qu'une fuite de la base ou
  d'une sauvegarde ne livre aucun second facteur et que le renouvellement de la clé des jetons
  n'en invalide aucun ([](/design/authentication/#the-second-factor)). Son réglage n'existe pas
  encore ; quand il arrivera, gardez-la à l'écart de la base de données et de ses sauvegardes,
  comme la clé des jetons.
- **L'inscription ouverte exige un quota par défaut** (prévu). Une fois les quotas configurables,
  `serve` refusera de démarrer si l'inscription par mot de passe est activée sans limite par
  défaut, et un opérateur qui n'en veut aucune la fixe explicitement à `unlimited`, ce qui est
  journalisé au niveau `WARN` à chaque démarrage ([](/design/quota/#thresholds-and-states)).
  Aujourd'hui, tous les déploiements fonctionnent sans limite.

`SYNC_CURSOR_MAC_KEY` est dérivée de `JWT_ED25519_DER` si elle n'est pas définie : renouveler la
clé des jetons invalide donc aussi les curseurs de synchronisation en circulation, sauf si vous la
définissez explicitement. La fédération est désactivée tant que `FEDERATION_ENABLED=true` et
`FEDERATION_URL` ne sont pas définis ; laissez-la désactivée.

Nommez votre proxy inverse pour que les limites de débit par source voient le client plutôt que
le proxy.

| Variable               | Signification                                                                                                                                      |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TRUSTED_PROXIES`      | Adresses et réseaux CIDR séparés par des virgules (`10.0.0.0/8`). Un réseau `/0` est refusé, car il ferait confiance à n'importe quel expéditeur.  |
| `TRUSTED_PROXY_HOPS`   | Fait confiance à ce nombre de sauts de transfert les plus proches du serveur, quelle que soit leur adresse.                                        |
| `TRUSTED_PROXY_HEADER` | Le seul champ de transfert que lit le serveur : `x-forwarded-for` (par défaut) ou `forwarded` (RFC 7239). Toute autre valeur empêche le démarrage. |

Si rien n'est défini, aucun en-tête de transfert n'est pris en compte. C'est sûr, mais tous les
clients derrière le proxy partagent alors le quota du proxy, si bien qu'un seul inconnu qui devine
des mots de passe ralentit la connexion pour tout le monde
([](/design/authentication/#sign-in-limits)) ; le serveur journalise un avertissement lorsqu'il
voit un en-tête de transfert alors qu'il ne fait confiance à aucun saut. L'erreur inverse est
pire : faire confiance à un saut qui n'ajoute pas l'adresse du client permet à chaque client de
choisir sa propre clé de limitation de débit. Le champ que `TRUSTED_PROXY_HEADER` ne nomme pas est
ignoré, quel qu'en soit l'expéditeur : un client ne peut donc pas choisir sa propre clé de
limitation en envoyant un champ que votre proxy transmet. Nommez le champ auquel votre proxy
ajoute l'adresse. Avec la valeur par défaut, faites ajouter par votre proxy l'adresse qu'il a vue
à `X-Forwarded-For`, comme le fait nginx avec :

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Ne définissez `TRUSTED_PROXY_HEADER=forwarded` que si votre proxy ajoute son propre élément à
`Forwarded` et n'écrit pas `X-Forwarded-For`.

<a id="migrate-then-start"></a>

## Migrer, puis démarrer

`serve` ne migre jamais et refuse un schéma de base de données plus ancien que le binaire.
Exécutez d'abord les migrations, à chaque mise à niveau :

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` journalise sur stderr et affiche `listening on <url>` sur stdout. N'utilisez pas
`--memory` en production : cette option garde tout, sauf les blobs, dans la mémoire du processus.

## Maintenance

Planifiez ces commandes ponctuelles (cron ou minuteur). Elles ont besoin de `DATABASE_URL` et de
`--blob-root`, et s'exécutent par défaut à blanc (dry run) :

- `capsule-server gc --apply` : récupère les blobs non référencés après un délai de grâce et
  supprime les sessions, sessions de téléversement et compteurs expirés. Lancez-la régulièrement,
  même quand l'espace de stockage n'est pas compté.
- `capsule-server purge --apply` : purge définitivement les éléments supprimés logiquement dont le
  délai de conservation est écoulé. Elle a aussi besoin de l'`ATTESTATION_KEY_SEED` du serveur,
  car chaque purge est d'abord signée dans le registre du propriétaire
  ([](/design/filesystem/server/#the-owner-ledger)), et refuse de s'exécuter sans elle. Elle se
  termine avec un code non nul lorsque le registre n'a pas pu enregistrer une purge ; l'élément
  concerné est conservé pour la prochaine exécution.
- `capsule-server scrub [--deep]` : vérification d'intégrité en lecture seule de l'index par
  rapport au stockage de blobs. Elle **se termine avec un code non nul à la moindre anomalie** :
  déclenchez une alerte sur ce code
  ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

Après la restauration d'une sauvegarde ou la perte de la base de données, reconstruisez l'index à
partir des registres des propriétaires sous `BLOB_ROOT`
([](/design/filesystem/server/#rebuild)). Définissez `ATTESTATION_KEY_SEED` sur l'hôte pour qu'il
vérifie les signatures des registres.

1. `capsule-server rebuild` compare l'index aux registres.
2. Avec `serve` arrêté, `capsule-server rebuild --apply` restaure les lignes qui manquent à
   l'index. Il n'écrase jamais une ligne que l'index contient. Il réancre un registre qui a perdu
   sa fin, après quoi le serveur écrit de nouveau pour ce propriétaire.
3. Si le rapport indique qu'un registre a été réécrit, le serveur refuse les écritures de ce
   propriétaire jusqu'à ce que vous ayez enquêté et lancé
   `capsule-server rebuild --apply --accept-rewrites`.

Un serveur qui a appliqué des opérations de cycle de vie (suppression, restauration, mise à jour
des métadonnées, dérivé, remplacement) avant d'enregistrer la taille de leurs blobs conserve ces
références avec une taille de 0. Lancez une fois `capsule-server repair-sizes` pour les lister,
puis `capsule-server repair-sizes --apply` pour renseigner chaque taille à partir du stockage de
blobs. C'est sans risque à côté d'un `serve` en cours d'exécution, et chaque élément réparé est
renvoyé aux clients. La commande se termine avec un code non nul lorsqu'un blob est manquant ou
vide, et laisse cette référence en l'état.

<a id="backups"></a>

## Sauvegardes

L'état durable se compose de PostgreSQL et de `BLOB_ROOT` (blobs et `ledger/`) ; sauvegardez les
deux, ensemble. La base de données contient aussi les sessions de connexion et les sessions de
téléversement en cours, si bien qu'une restauration garde les utilisateurs connectés. Conservez
aussi les variables de clés ci-dessus dans votre coffre à secrets : elles ne sont pas dans la base
de données, et l'identité de signature et d'attestation du serveur en dépend.

Sauvegardez d'abord la base de données, puis les blobs :

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` peut continuer à tourner. Un blob est écrit sur le disque et son entrée de registre
ajoutée avant que la ligne d'index qui le nomme ne soit validée : une copie des blobs faite après
le dump contient donc tous les blobs que le dump référence, et éventuellement des écritures que le
dump a manquées. N'exécutez ni `gc --apply` ni `purge --apply` pendant la sauvegarde : l'un comme
l'autre peut supprimer un blob que le dump référence encore.

La reconstruction ci-dessous restaure les éléments téléversés entre le dump et la copie des blobs,
car elle ajoute chaque ligne qui manque à l'index. Elle ne modifie jamais une ligne que le dump
contient déjà : une suppression, une restauration ou une modification de métadonnées faite sur un
élément plus ancien pendant cet intervalle n'est donc pas appliquée. Une reconstruction signale un
tel changement comme une ligne divergente (et se termine avec un code non nul) uniquement s'il a
modifié les blobs de l'élément ; une suppression ou une restauration passe inaperçue. Lorsqu'une
sauvegarde doit capturer chaque changement, arrêtez `serve` pendant que vous prenez les deux
moitiés. Si une restauration signale des lignes divergentes et que l'ancien hôte tourne encore,
refaites la sauvegarde de cette manière.

<a id="restoring-onto-a-new-host"></a>

### Restaurer sur un nouvel hôte

1. Démarrez un PostgreSQL vide, et définissez les mêmes `JWT_ED25519_DER` et
   `ATTESTATION_KEY_SEED` que sur l'ancien hôte.
2. Restaurez la base de données. Le dump contient le schéma et son historique de migrations : ne
   lancez donc pas `capsule-server-migration up` au préalable :
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Recopiez le répertoire de blobs dans `BLOB_ROOT`, sur un seul système de fichiers.
4. Avec `serve` arrêté, lancez `capsule-server rebuild` et lisez son rapport. Il liste les
   éléments téléversés après le dump ; `capsule-server rebuild --apply` les restaure avec les
   numéros de séquence qu'ils avaient, si bien que les curseurs de synchronisation des clients
   restent valides.
5. Lancez `capsule-server scrub --deep`. Il ne devrait rien signaler ; il se termine avec un code
   non nul à la moindre anomalie.
6. Démarrez `serve`. Les sessions reviennent avec la base de données : un client connecté au
   moment du dump le reste, sauf si sa session a expiré depuis. Avant de se mettre à l'écoute,
   `serve` abandonne chaque session de téléversement inachevée dont le fichier temporaire sous
   `BLOB_ROOT` n'a pas été restauré, et supprime chaque fichier temporaire sans session ; un client
   dont le téléversement a été abandonné recommence ce fichier. Les clients reprennent la
   synchronisation là où ils l'avaient laissée.

Si la base de données est perdue et qu'il n'existe aucun dump, migrez une base vide et lancez
`capsule-server rebuild --apply` sur le répertoire de blobs. Il restaure chaque ligne d'élément à
partir des registres des propriétaires. Les comptes, les annuaires d'appareils publiés,
l'appartenance aux albums et les liens de partage ne se trouvent que dans la base de données, et
une reconstruction ne les restaure pas.

Répétez cette procédure avant de lui confier de vraies données, par exemple en vérifiant avec
`capsule restore` que chaque original revient octet pour octet.
`capsule-e2e/tests/backup_restore_drill.rs` exécute les mêmes étapes sur des conteneurs
(`mise run test-rust-containers`).
