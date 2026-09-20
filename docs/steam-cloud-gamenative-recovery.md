# GameNative: reprise d'une synchronisation Cloud sans confirmation

Recherche du 19 septembre 2026 sur le code public, sans exécution, connexion Steam ni modification de sauvegardes. Périmètre: synchronisation Kotlin AutoCloud. Le comportement du runtime natif propriétaire n'est pas établi ici.

GameNative reprend une synchronisation complète après certains échecs, y compris au prochain lancement. Il n'a pas, dans ce chemin, de procédure séparée pour retrouver le résultat d'un ancien commit incertain. Cela établit un précédent pour une reprise simple, sans prouver son succès dans toutes les situations serveur.

## Versions vérifiées

- GameNative `master`, SHA [`c3b9f9e5de038b8caf2d049f8ef261d56ce642f3`](https://github.com/utkarshdalal/GameNative/tree/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3), vérifié sur GitHub et par `git ls-remote`.
- La [configuration Gradle](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/gradle/libs.versions.toml#L15) déclare JavaSteam `1.8.0.1-26-SNAPSHOT` et référence `joshuatam/JavaSteam`, branche `gamenative-latest`. Cette branche a été inspectée au SHA [`433f2ad15c36d5e690a4fe77401ec3f6b960641e`](https://github.com/joshuatam/JavaSteam/tree/433f2ad15c36d5e690a4fe77401ec3f6b960641e). L'artefact SNAPSHOT exact utilisé par une application distribuée n'a pas été vérifié.
- Source téléchargée sous `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-gamenative-cloud-recovery-4voi4rew/`; pointeur local `build/gamenative-cloud-recovery-source.txt`.

## Échec et nouvelle tentative

Au lancement, en synchronisation manuelle et à la fermeture, `SteamService` effectue jusqu'à trois tentatives de synchronisation complète sur `AsyncJobFailedException`, avec une attente d'une puis deux secondes. Au lancement, l'épuisement donne `UnknownFail`. À la fermeture, il est journalisé. Les autres exceptions n'entrent pas dans cette boucle. Sources: [lancement](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamService.kt#L3127-L3199), [manuel et fermeture](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamService.kt#L3221-L3322).

[`asyncIsolated`](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/utils/CoroutineUtils.kt#L8-L18) transmet l'exception d'origine au `Deferred`. Il ne transforme pas tous les problèmes réseau en `AsyncJobFailedException`. L'[appelant de fermeture](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/model/MainViewModel.kt#L851-L863) journalise les exceptions restantes, sauf l'annulation qu'il propage.

L'interface regroupe `UnknownFail`, `DownloadFail` et `UpdateFail` dans une [modal d'échec avec OK](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/PluviaMain.kt#L2715-L2727). [OK ferme la modal](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/PluviaMain.kt#L969-L976); un prochain lancement [rappelle la synchronisation](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/PluviaMain.kt#L2631-L2644).

## Ce qui survit à l'échec

L'état Room contient un [numéro de changement par jeu](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/data/ChangeNumbers.kt#L6-L10) et une [liste de fichiers de référence](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/data/FileChangeLists.kt#L6-L10), avec [chemins, dates et SHA](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/data/UserFileInfo.kt#L15-L24).

Le transfert attend le commit puis la clôture du batch. Le `batchID` reste local à la fonction; ce chemin ne journalise pas durablement la requête incertaine. La référence Room n'avance qu'après le retour réussi de l'upload. Une exception avant ce point conserve donc l'ancienne référence. [Upload](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L738-L772), [mise à jour Room](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L893-L926).

Le fichier `pending_achievement_sync.txt` concerne les succès hors ligne. Son [traitement à la reconnexion](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamService.kt#L4745-L4825) n'est pas une reprise de batch Cloud.

## Au prochain essai

Chaque essai [relit la liste Cloud et son numéro](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L802-L824). Déduction du branchement:

- Cloud inchangé et fichiers locaux modifiés: nouvel upload.
- Cloud avancé et fichiers locaux modifiés depuis l'ancienne référence: conflit.
- Ainsi, un upload réussi dont la confirmation manque peut provoquer un conflit au nouvel essai, même si local et Cloud sont identiques. L'égalité directe local/Cloud est utilisée seulement quand le cache est absent ou vide.

Sources: [branche Cloud avancé](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L962-L1055), [numéros égaux](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L1059-L1076).

Le choix local ou Cloud [relance la préparation avec cette préférence](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/PluviaMain.kt#L937-L966). Après une synchronisation réussie, `signalAppLaunchIntent` vérifie encore les [opérations distantes en attente](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamService.kt#L3150-L3177). Un upload distant en cours donne une modal avec OK; un upload en attente peut proposer de jouer quand même. [Présentation](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/ui/PluviaMain.kt#L2740-L2778).

## Limites de ce précédent

Le booléen `commitSuccess` est journalisé mais n'affecte pas `uploadBatchSuccess`; un retour `false` n'est donc pas traité comme un échec à cet endroit. [Code](https://github.com/utkarshdalal/GameNative/blob/c3b9f9e5de038b8caf2d049f8ef261d56ce642f3/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L738-L772).

Dans la branche JavaSteam référencée, [commit et clôture](https://github.com/joshuatam/JavaSteam/blob/433f2ad15c36d5e690a4fe77401ec3f6b960641e/src/main/java/in/dragonbra/javasteam/steam/handlers/steamcloud/SteamCloud.kt#L281-L322) attendent les réponses sans procédure de récupération spéciale. Le [timeout de base des jobs](https://github.com/joshuatam/JavaSteam/blob/433f2ad15c36d5e690a4fe77401ec3f6b960641e/src/main/java/in/dragonbra/javasteam/types/AsyncJob.kt#L14-L32) vaut dix secondes et peut être prolongé par un heartbeat.

Cette lecture prouve la stratégie du client public. Elle ne prouve ni l'annulation d'un ancien batch au timeout, ni l'idempotence des commits, ni la visibilité immédiate de toute opération Steam. Aucun essai de perte de confirmation n'a été exécuté. Une approche similaire pour Forge serait un choix de reprise à valider, pas une obligation imposée par Steam.
