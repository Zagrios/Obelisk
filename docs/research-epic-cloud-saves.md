# Sauvegardes cloud Epic dans Forge

Recherche du 21 septembre 2026. Sources publiques uniquement, sans connexion au compte Epic ni accès aux sauvegardes réelles.

## Deux services distincts

**Epic Games Launcher Cloud Saves** correspond fonctionnellement à Steam AutoCloud. L'éditeur configure les dossiers PC et Mac, les fichiers à synchroniser et les quotas dans le Developer Portal. Le launcher synchronise après la fermeture du jeu et restaure les données avant de jouer. Epic documente aussi les conflits et leurs tests. [Documentation officielle Cloud Saves](https://dev.epicgames.com/docs/epic-games-store/services/cloud-save).

**EOS Player Data Storage** est un stockage appelé par le code du jeu, comparable au rôle fonctionnel d'une API comme Steam RemoteStorage. Epic précise explicitement que ce service est différent des Cloud Saves du launcher. Les données sont propres au joueur et au jeu, chiffrées avec une clé fournie par le jeu et absente du serveur Epic. L'API reçoit notamment un `EOS_ProductUserId`. Le jeu pilote les opérations et peut partager les données entre plateformes. [Documentation officielle, dont la section PlayerDataStorage vs EGS Cloud Saves](https://dev.epicgames.com/docs/epic-online-services/player-and-game-data/player-data-storage-interface/player-data-storage-overview).

Conséquence pour Forge : implémenter les Cloud Saves du launcher est un chantier identifiable. Ajouter le SDK EOS à Forge ne donne pas un accès générique au stockage EOS de tous les jeux. Pour un jeu qui utilise EOS, sa propre intégration reste responsable des sauvegardes ; son bon fonctionnement au lancement depuis Forge devra être vérifié séparément. La configuration EOS comprend des identifiants de produit, sandbox, déploiement, client et une clé propre au jeu. [Configuration EOS officielle](https://dev.epicgames.com/documentation/unreal-engine/enable-and-configure-online-services-eos-in-unreal-engine).

## Ce que prouve Legendary

Le code ci-dessous est une implémentation indépendante du protocole du launcher, utilisée comme référence de rétro-ingénierie. Ce n'est pas une API publique garantie par Epic. Révision examinée : `42f6bdeadde3a9526dc8eb713763476999ac217b`.

- Les métadonnées contiennent `CloudSaveFolder`, `CloudSaveFolder_MAC`, `CloudIncludeList` et `CloudExcludeList`. Pour un jeu Mac natif, Legendary choisit explicitement le champ Mac et résout les variables de chemin. Son commentaire sur `{appdata}` signale même un écart observé pour Loop Hero, donc ces règles demandent des vérifications par jeu. [Résolution des chemins](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/core.py#L990-L1032), [filtres](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/core.py#L1145-L1166).
- Le service `datastorage` expose un inventaire sous `/api/v1/access/egstore/savesync/{account_id}/{app_name}/`. Legendary utilise GET pour lister et POST pour préparer des fichiers, puis des liens `readLink` et `writeLink` pour transférer les octets. [API cliente](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/api/egs.py#L281-L305), [transferts](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/core.py#L1171-L1278).
- Le format utilise des manifestes et chunks BuildPatch, avec noms relatifs, SHA-1 des fichiers, références aux chunks et métadonnées CloudSaveFolder. Les manifestes sont datés. [Construction du contenu](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/utils/savegame_helper.py#L64-L179).
- La décision local/distant de Legendary compare les dates avec une tolérance de 60 secondes. Le code reconnaît que cela reste une estimation. Forge devrait conserver sa politique de comparaison des contenus et d'historique de synchronisation, plutôt que reprendre cette heuristique. [Comparaison](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/core.py#L1115-L1143).

Cette lecture prouve l'existence d'une implémentation du protocole. Elle ne prouve pas sa disponibilité actuelle pour le compte de l'utilisateur, ni la compatibilité des sauvegardes entre deux versions d'un jeu.

## Travail à prévoir dans Forge

L'inspection locale menée en parallèle confirme que `CatalogItem` dans `crates/storefronts/epic-kit/src/client.rs` ne conserve pas encore `customAttributes`. Le module `content_manifest.rs` sait lire les manifestes et le contenu sait décoder les chunks, mais le chemin cloud d'écriture reste à créer. Le lancement Epic dans `EpicModel.swift` n'a pas de synchronisation avant/après ; `EpicGameLauncher` dans `SteamGameLauncher.swift` abandonne le résultat `LaunchedGameProcess`. Les types et chemins persistants de `steam-cloud` contiennent encore `steam_id` et `cloud_app_id`.

Plan proposé, sans implémentation dans cette recherche :

1. Conserver les capacités et règles cloud Epic, résoudre les chemins Mac et appliquer strictement les filtres.
2. Ajouter un adaptateur Rust Epic pour inventaire, téléchargement, reconstruction, génération de manifestes/chunks et envoi vérifié.
3. Réutiliser la logique de réconciliation, conflits, sauvegarde locale et reprise en généralisant l'identité du fournisseur et du compte. Ne pas partager aveuglément les chemins persistants Steam.
4. Superviser la session Epic côté Rust pour restaurer avant lancement, attendre la fin effective du jeu, envoyer ensuite et reprendre après interruption de Forge.
5. Valider d'abord hors ligne les formats, chemins, filtres et conflits. Ensuite, sur un jeu Mac compatible explicitement choisi et avec autorisation, prouver un aller-retour avec sauvegarde modifiée entre Forge et un autre client Epic, avec sauvegarde préalable et relecture complète des octets distants.

La première cible raisonnable est la parité d'expérience avec AutoCloud pour les jeux Epic déclarant le support Mac. La parité universelle avec les jeux utilisant EOS Player Data Storage reste un autre périmètre. Aucun test authentifié, essai de jeu ou transfert cloud n'a été réalisé pour cette note.
