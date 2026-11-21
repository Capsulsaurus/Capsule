---
title: Guide d'utilisation
description: L'usage quotidien d'une bibliothèque Capsule avec la CLI capsule — importer, sauvegarder, vérifier, organiser et supprimer en toute sécurité.
status: draft
---

Ce guide couvre la CLI `capsule`, le client de référence. Les commandes marquées *(prévu)*
figurent sur la [feuille de route](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)
et ne sont pas encore disponibles. `capsule <command> --help` fait foi pour les options ; la
référence de la CLI ([](/reference/cli/)) les liste toutes.

:::caution
Capsule est en alpha. Jusqu'à la première version de dogfooding, conservez une sauvegarde
indépendante de chaque original que vous importez.
:::

<a id="set-up"></a>

## Mise en place

1. Créez une bibliothèque : `capsule library init --name "Photos"`. La bibliothèque est un
   répertoire local chiffré ; sa phrase secrète protège les clés sur cette machine.
2. Connectez-vous à votre serveur : `capsule auth register` ou `capsule auth login` (`--oidc` ou
   `--device` pour l'authentification unique).
3. Configurez la récupération : `capsule auth escrow`. Conservez hors ligne le secret de
   récupération affiché. Sans lui et sans votre phrase secrète, une machine perdue signifie une
   bibliothèque perdue, car le serveur ne peut rien déchiffrer.

<a id="import-and-back-up"></a>

## Importer et sauvegarder

- `capsule import <path>` copie les fichiers dans la bibliothèque, signe la provenance et les
  métadonnées de chaque élément et génère les miniatures. `--provider takeout` importe une
  archive Google Takeout avec ses métadonnées annexes ([](/guides/google-photos-migration/)).
- `capsule push` téléverse la bibliothèque. Les téléversements reprennent là où ils se sont
  arrêtés, si bien que relancer `push` est toujours sans risque. Après chaque élément, `push`
  demande au serveur de confirmer qu'il conserve durablement chaque blob et garde dans la
  bibliothèque les reçus de conservation signés par le serveur. Un élément en échec est signalé
  et l'exécution continue. La commande se termine avec un code non nul si un élément a échoué, et
  `--json` liste le résultat de chaque élément.
- `capsule import --push` fait les deux en une seule étape.
- **Gardez vos originaux tant que la conservation n'est pas vérifiée.** Un élément n'est stocké
  en sécurité qu'une fois que le serveur l'a déclaré durable et a émis un reçu de conservation
  signé, ce que `push` signale comme vérifié. Ne supprimez pas les fichiers sources avant.
  `import --move --push` ne supprime chaque source qu'après la vérification de son élément. Sans
  `--push`, `--move` conserve toutes les sources et l'indique ; un `import --move --push` ultérieur
  sur les mêmes fichiers les libère.
- `capsule restore` *(prévu)* télécharge les originaux et les compare octet pour octet à ce qui a
  été signé lors de l'import.

<a id="organize"></a>

## Organiser

- Les **étiquettes** sont libres et s'appliquent à un élément, à une pile (paire RAW+JPEG,
  rafale, bracketing), à un album ou à une région d'une image. Les étiquettes que vous ajoutez sont
  conservées séparément de celles suggérées par l'IA, et vous pouvez promouvoir ou écarter une
  suggestion. Les étiquettes d'élément existent aujourd'hui ; celles de pile, d'album et de région
  sont *(prévues)*.
- `capsule show <asset>` affiche ce que les métadonnées signées d'un élément enregistrent :
  légende, note, étiquettes, lieu, date de prise de vue.
- `capsule repair capture-time` trouve les éléments dont la date de prise de vue ne concorde pas
  avec l'EXIF de l'original et, avec `--apply`, les corrige. La modification du lieu est
  *(prévue)*.

Chaque modification est un changement signé, en ajout seul : la valeur précédente reste dans
l'historique de provenance de l'élément, et les modifications simultanées depuis deux appareils
fusionnent de manière déterministe.

<a id="delete-safely"></a>

## Supprimer en toute sécurité

Corrigez les métadonnées comme la géolocalisation avant de supprimer, tant que les éléments
voisins vous donnent encore du contexte. Une photo sans lieu propre est facile à situer tant que
ses voisines de la même promenade sont dans la bibliothèque ; une fois qu'elles ont disparu, ce
contexte aussi.

- `capsule cull` marque les éléments comme sélectionnés, neutres ou rejetés, et filtre selon ce
  marquage.
- `capsule cull --sweep` déplace les rejets vers la corbeille. Les éléments mis à la corbeille
  restent récupérables pendant le délai de conservation (`--retain-days`) avant d'être purgés.
- `capsule trash delete --asset <id>` place un élément dans la corbeille sur le serveur comme dans
  la bibliothèque. Un élément déjà dans la corbeille, par exemple après `capsule cull --sweep`,
  garde le délai de conservation avec lequel il y a été placé : `--retain-days` ne s'applique qu'à
  une nouvelle suppression, et `--json` indique le délai envoyé dans `retention_until`.
- `capsule trash restore --asset <id>` restaure un élément. Une restauration après la fin du
  délai de conservation est refusée (`retention_elapsed` avec `--json`) et n'est pas retentée ;
  l'élément reste dans la corbeille jusqu'à sa purge.

<a id="share"></a>

## Partager

- `capsule share create --album <id>` ou `--asset <id>` émet un lien en lecture seule ;
  `--protect` ajoute une phrase secrète au lien et `--expires-at` une date d'expiration. Le lien
  porte sa clé dans le fragment de l'URL, qui ne parvient jamais au serveur.
- `capsule share revoke <id>` empêche un lien de s'ouvrir.

<a id="sync-another-machine"></a>

## Synchroniser une autre machine

- `capsule sync` récupère depuis le serveur le flux de modifications de la bibliothèque ;
  `capsule list` montre ce qu'il a apporté.
- `capsule auth recover` installe le compte sur une nouvelle machine à partir du secret de
  récupération.
