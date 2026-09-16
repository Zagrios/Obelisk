# Implémenter les amis Epic en temps réel dans Forge

Travaille dans `/Users/mathieu/Documents/Github/Zagrios/forge`. Implémente la fonctionnalité dans le code, avec ses tests et sa vérification visuelle. Les choix produit ci-dessous sont validés. Avance sans redemander leur confirmation et réponds en français.

## Objectif

Afficher les vrais amis du compte Epic connecté, leur présence et leur jeu en cours dans la même modale que les amis Steam. Les mises à jour doivent arriver par événements, sans polling périodique des amis ou des statuts, y compris lorsque la modale est fermée.

Réutilise l'authentification Epic native de Forge. L'utilisateur doit pouvoir installer seulement Forge, sans installer le launcher Epic, Wine ou un programme externe pour cette fonctionnalité. Garde les composants réutilisables pour GOG plus tard.

Cette tâche couvre la consultation des amis et des statuts. Les invitations à jouer, demandes d'amitié à accepter ou envoyer, discussions, lancement des jeux, succès, temps de jeu et synchronisation cloud restent des tâches distinctes. Les changements de relation effectués ailleurs doivent néanmoins actualiser la liste. Aucun bouton factice pour les fonctions futures.

## 1. Vérifier l'existant

Lis les instructions du dépôt et vérifie l'état Git. Préserve les changements et fichiers préexistants, y compris ceux qui ne sont pas suivis. Ne fais aucun commit, push ou publication.

Constats vérifiés le 16 septembre 2026, à confirmer dans le checkout actuel :

- Forge utilise SwiftUI/AppKit, Rust et UniFFI.
- Les amis Steam disposent déjà de composants communs, d'un état de présentation indépendant du protocole et d'une observation asynchrone.
- `RootView` ouvre encore la modale et active son cache d'avatars en se référant directement à `model.steam`.
- `EpicModel` gère la session, les générations de compte et la sauvegarde des identifiants renouvelés, mais ne possède pas encore l'observation des amis.
- `epic-kit` possède une session OAuth du launcher et son renouvellement sérialisé. L'accès à la bibliothèque ne prouve pas l'accès aux services sociaux.
- La déconnexion Epic actuelle annule les téléchargements et révoque le jeton OAuth. Elle ne convient pas pour reconnecter uniquement le transport social.

Fichiers à examiner sous la racine du dépôt :

| Partie | Fichiers |
| --- | --- |
| Authentification et client Epic | `crates/storefronts/epic-kit/src/auth.rs`, `client.rs`, `lib.rs` et `Cargo.toml` dans le même kit |
| Pont Rust/Swift | `crates/forge-bridge/src/epic.rs`, `steam.rs`, `lib.rs`, puis les protocoles générés dans `apps/macos/Forge/Bridge/Generated/` |
| Session Epic et stockage | `apps/macos/Forge/Models/EpicModel.swift`, `apps/macos/Forge/Bridge/EpicKeychain.swift` |
| Référence d'observation Steam | `apps/macos/Forge/Models/SteamModel.swift`, `apps/macos/Forge/Bridge/SteamSessionConnection.swift` et le service d'amis de `steam-kit` |
| Présentation commune | `apps/macos/Forge/Models/FriendsPresentation.swift`, `apps/macos/Forge/DesignSystem/FriendsPanel.swift`, `FriendsModal.swift` et le cache d'avatars existant |
| Ouverture et durée de vie | `apps/macos/Forge/Features/RootView.swift`, `apps/macos/Forge/App/AppModel.swift`, `apps/macos/Forge/Features/Epic/EpicView.swift` |
| Tests | `apps/macos/ForgeTests/EpicModelTests.swift`, `EpicIntegrationTests.swift`, `EpicDownloadTests.swift`, `FriendsPresentationTests.swift`, `SteamFriendsIntegrationTests.swift` et les tests Rust |
| Documentation | `README.md`, `docs/architecture.md`, `docs/epic-protocol.md`, `docs/research-epic-friends.md` |
| Génération et vérification | `scripts/build-rust.sh`, `scripts/generate-project.sh`, `scripts/test.sh`, `scripts/verify-localization.py` |

Le prompt Steam historique explique le contexte, mais le code actuel prime sur ses descriptions antérieures à l'implémentation. Préserve le comportement Steam et les opérations Epic de bibliothèque, téléchargement, vérification et lancement.

## 2. Valider le protocole Epic avant de figer l'intégration

La piste à vérifier est le protocole social du launcher : chargement initial par REST, puis STOMP sur WebSocket pour les changements.

Une implémentation indépendante existe dans EGL3. Ses fichiers de transport social datent de 2021. Ils servent de référence de protocole, sans garantir que les services actuels acceptent la session Forge ni que tous les schémas sont inchangés.

- Le [client REST EGL3](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/EpicClientAuthed.cpp) utilise `/friends/api/v1/<accountId>/summary?displayNames=true`. Son [modèle de réponse](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/responses/GetFriendsSummary.h) distingue amis acceptés, demandes entrantes et sortantes, suggestions et blocages. Vérifie l'hôte dans les sources et toute pagination actuelle.
- Les [hôtes EGL3](https://github.com/WorkingRobot/EGL3/blob/master/src/web/Hosts.h) distinguent le launcher STOMP sur `wss://connect.ol.epicgames.com/`, EOS STOMP et Fortnite XMPP.
- Le [transport STOMP EGL3](https://github.com/WorkingRobot/EGL3/blob/master/src/web/stomp/StompClient.cpp) authentifie le WebSocket avec le jeton du launcher dans l'en-tête `Authorization`, puis établit la session STOMP. Vérifie le format exact attendu, les sous-protocoles, la négociation et l'abonnement.
- Le [client d'amis EGL3](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/friends/FriendsClient.cpp) s'abonne à une destination terminée par `/account/<accountId>` et traite notamment `core.connect.v1.connected`, `presence.v1.UPDATE`, `friends.v1.FRIEND_ENTRY_ADDED`, `FRIEND_ENTRY_REMOVED`, `FRIEND_ENTRY_UPDATED`, ainsi que les événements de demandes et de blocage.

L'interface officielle EOS offre des notifications d'amis et de présence, mais sa liste dépend du consentement des amis à l'application EAS. Créer une application EOS Forge ne donne pas automatiquement accès à tous les amis visibles dans le launcher. Ne remplace pas cet objectif par une liste limitée à un jeu ou à une nouvelle application. [Explication officielle Epic](https://onlineservices.epicgames.com/news/querying-for-epic-friends-and-their-status)

Commence par établir les points suivants avec le chemin de session normal de Forge :

1. Le service renvoie la vraie liste des amis acceptés du compte connecté.
2. Le WebSocket accepte l'authentification, l'abonnement est établi et les messages reçus sont correctement identifiés.
3. Une méthode vérifiée fournit la présence initiale des amis. Une liste de relations REST et une socket ouverte ne suffisent pas. Identifie la livraison initiale, une souscription nécessaire ou une requête ponctuelle documentée par les sources ou observations. N'invente pas d'endpoint.
4. Un changement réel de statut arrive spontanément. Vérifie ensuite les transitions de jeu et la couverture disponible selon les produits Epic. Distingue les limites de visibilité du compte des erreurs d'intégration.
5. Les effets de l'ouverture de cette session sur la présence du compte utilisateur sont compris. EGL3 publie lui-même une présence lors de l'établissement de la connexion. Ne recopie pas automatiquement cette publication ni un statut de jeu. Préserve les préférences de présence et documente tout enregistrement de connexion nécessaire au protocole.

Ne suppose pas que les connexions EOS et Fortnite d'EGL3 sont toutes nécessaires, ni que le launcher seul couvre forcément chaque cas. Choisis le transport minimal justifié par la validation. Réutilise les dépendances Rust adaptées déjà présentes avant d'en ajouter.

Si le protocole a changé, cherche une référence primaire plus récente et adapte l'implémentation. Si l'accès social est refusé ou si une validation réelle nécessite une interaction utilisateur indisponible, poursuis les parties testables indépendantes et indique précisément la limite. Ne présente pas des mocks ou une socket connectée comme preuve d'une intégration Epic fonctionnelle. Aucun repli silencieux vers du polling.

## 3. Réutiliser la présentation Steam

Ajoute au menu du compte Epic une entrée localisée du type « Amis · 4 en ligne ». Le compteur représente l'ensemble des amis connectés, indépendamment de la recherche. Pendant le chargement ou une coupure, affiche l'état correspondant au lieu d'un zéro présenté comme certain.

Réutilise `FriendsPresentation`, `FriendsPanel`, `FriendsModal` et le cache d'avatars. La modale Epic conserve le style et les interactions Steam : centrage dans toute la fenêtre, logo et titre Epic, compteur, recherche locale, avatars ou repli, pseudo, statut et jeu connu. Les amis en jeu passent avant les autres connectés. L'ordre reste stable, les hors ligne restent recherchables et la fermeture au clavier conserve son comportement.

Remplace le branchement Steam spécifique de `RootView` par une seule modale active identifiée par le magasin et le compte. Cet état est indépendant de la bibliothèque sélectionnée. Steam et Epic doivent pouvoir rester connectés simultanément et conserver chacun leur flux d'amis. Associe l'identité de la modale et le cache d'avatars au compte effectivement présenté.

Garde les modèles de protocole Epic dans l'adaptateur Epic. Utilise les identifiants Epic comme des chaînes opaques et une identité de présentation composée du magasin, du compte et de l'ami. Le futur GOG doit pouvoir utiliser les mêmes composants sans implémentation anticipée de son backend.

Préserve les états chargement, liste vide, aucun résultat, présence inconnue, erreur et connexion interrompue. Une coupure conserve éventuellement les dernières données en signalant qu'elles ne sont plus actualisées. Les avatars et jeux inconnus utilisent les replis existants, sans URL inventée ni dépendance à la possession du jeu par l'utilisateur.

## 4. Ajouter l'état et les événements dans Rust

Implémente dans `epic-kit` un état social associé à la session, alimenté par REST et par les événements du transport. Distingue les relations d'amitié, la présence, l'activité et les métadonnées de présentation.

Règles de cohérence :

- Afficher seulement les amis acceptés. Exclure le compte lui-même, les suggestions, les comptes bloqués et les demandes en attente. Une acceptation, suppression ou modification effectuée ailleurs doit être prise en compte.
- Une liste complète remplace les relations, y compris si elle est vide. Les événements incrémentaux modifient uniquement les entrées concernées. Une présence isolée ne crée pas une amitié.
- Éviter de perdre ou d'écraser un événement pendant le chargement REST. Prévoir un abonnement effectif, un tampon borné et une réconciliation selon les garanties constatées du protocole. Un débordement exige une resynchronisation. Ne suppose pas un ordre global entre HTTP et WebSocket.
- Gérer les mises à jour partielles selon le schéma vérifié. Un champ absent, un effacement explicite et une valeur inconnue ont des sens différents. Une suppression d'ami ne doit pas être annulée par une ancienne présence ou une réponse de métadonnées tardive.
- Le [modèle de présence EGL3](https://github.com/WorkingRobot/EGL3/blob/master/src/web/stomp/messages/Presence.h) contient un statut de compte et plusieurs présences par namespace, avec activité et connexions. Vérifie si chaque message remplace ou modifie ces données. Agrège les sessions de façon déterministe et teste la fermeture d'une seule connexion lorsqu'une autre reste active.
- Sépare la disponibilité de l'activité. Un ami absent peut avoir un jeu lancé. À la fermeture d'un jeu ou lors d'un passage autoritatif hors ligne, retire l'activité périmée. Elle ne réapparaît pas sans nouvelle donnée valide.
- Respecte les données de visibilité retournées. Une présence inconnue n'est pas hors ligne et un statut masqué ne doit pas révéler un ancien jeu. Une connexion interrompue n'est pas un changement de présence des amis.

Le transport doit rester réactif pendant les téléchargements. Son lecteur ne doit pas attendre SwiftUI, une image ou une requête de métadonnées. Borne les files, tailles de messages et tâches d'enrichissement. Déduplique les données inchangées et mets les métadonnées en cache.

Pour le décodage STOMP, suis la version négociée et sa spécification : délimitation des trames, longueur du corps en octets, échappement des en-têtes, heartbeats et erreurs. Ne suppose pas une correspondance automatique entre message WebSocket et trame STOMP. Teste les assemblages applicables au transport. Une erreur de trame impose le traitement prévu par le protocole ; un événement métier inconnu peut être ignoré sans casser la bibliothèque. [Spécification STOMP 1.2](https://stomp.github.io/stomp-specification-1.2.html)

## 5. Observer les changements à travers UniFFI

Reprends le principe utilisé pour Steam : un état initial et une observation suspendue jusqu'à une nouvelle révision, ou un mécanisme équivalent fiable. Les noms exacts restent à choisir selon le code existant. Un équivalent Epic de `nextFriends(afterRevision:)` convient.

Chaque état porte son compte et une révision. La souscription et la lecture doivent éviter de perdre un changement intervenu entre les deux. Une reconnexion ou un changement de génération ne doit pas laisser un observateur attendre une ancienne révision indéfiniment.

Une boucle Swift qui attend réellement un changement convient. Une boucle avec timer ou `Task.sleep` relisant régulièrement le cache Rust ne convient pas. Les heartbeats, délais d'authentification et de reconnexion ont un autre rôle.

Ne conserve aucun verrou de session ou d'authentification pendant les lectures HTTP sociales, l'attente WebSocket ou l'observation des changements. Préserve la sérialisation nécessaire au renouvellement OAuth existant. Un observateur lent ne doit pas bloquer la socket. La fusion de snapshots complets est possible ; la perte silencieuse de deltas ne l'est pas.

Prévois une annulation explicite des tâches Rust. Le pont existant signale que l'annulation d'une `Task` Swift ne suffit pas à annuler une attente Rust. La déconnexion du compte et la destruction du service doivent réveiller les observateurs et terminer les connexions en attente. Une reconnexion sociale possède son propre mécanisme d'arrêt, sans appeler la déconnexion OAuth.

`EpicModel` convertit les données Epic vers la présentation commune et les publie sur le MainActor. Vérifie le compte et les générations de session et d'observation avant et après les attentes. Permets l'injection des sessions ou modèles Epic dans les tests, comme nécessaire dans `AppModel`, et mets à jour tous les doubles conformes aux protocoles modifiés.

## 6. Session, renouvellement et reconnexion

La session possède le service social. Le démarrage suit une connexion ou restauration Epic réussie. Fermer la modale, changer de section ou réouvrir le panneau ne crée pas une nouvelle connexion. Le compteur reste actualisé en arrière-plan.

Réutilise le renouvellement OAuth sérialisé de `epic-kit` afin que bibliothèque, téléchargements et amis ne lancent pas des renouvellements concurrents. Prévois la réaction du WebSocket à l'expiration ou au rejet du jeton selon le comportement vérifié.

Le refresh token renouvelé doit être sauvegardé rapidement avec le mécanisme Keychain existant lorsque la mémorisation est activée. Cette sauvegarde ne doit dépendre ni d'une actualisation de bibliothèque, ni du prochain événement d'ami, ni du succès ultérieur du chargement social. Conserve la sérialisation des écritures et les protections contre une ancienne génération. Respecte également `rememberSession = false`.

Les access tokens restent dans Rust. Réutilise les chemins prévus pour la persistance des identifiants, sans les afficher ni les copier dans des scripts, captures, fixtures, fichiers de diagnostic ou commandes shell. Les traces utiles décrivent les étapes, codes d'erreur et compteurs sans secrets ni contenu social brut.

Une erreur du service social reste distincte d'un incident de bibliothèque. Distingue réseau indisponible, limitation de débit, autorisation sociale refusée et authentification réellement expirée ou révoquée. Un refus de permission ne doit pas automatiquement déconnecter un compte encore valide pour la bibliothèque.

Prévois des délais progressifs de reconnexion, bornés et annulables, un traitement du réveil macOS et une resynchronisation. Respecte les indications de limitation de débit. Après épuisement des tentatives, affiche une action de nouvelle tentative sans boucle active.

À la déconnexion volontaire ou au changement de compte, ferme la modale concernée, annule le service et ses reprises, puis purge les données et images du compte précédent. Rejette toute réponse tardive. La déconnexion d'Epic ne doit pas interrompre le service Steam.

## 7. Localisation, génération et documentation

Tous les textes visibles, y compris erreurs et accessibilité, passent par les conventions `L10n` et `Localizable.xcstrings`. Réutilise les clés communes et ajoute les traductions anglaises et françaises nécessaires. Conserve le repli anglais pour les langues non prises en charge et les conventions `backend.*` pour les erreurs Rust présentées à l'utilisateur.

Régénère les bindings avec les scripts du dépôt. Les fichiers `Bridge/Generated` ne s'éditent pas manuellement. Mets à jour le projet Xcode si de nouveaux fichiers le nécessitent et préserve la configuration de signature.

Mets à jour en anglais la documentation technique concernée, notamment `docs/epic-protocol.md`, `docs/architecture.md` et la description de la fonctionnalité dans le README. Distingue ce qui est implémenté, testé automatiquement et observé sur les services Epic. Cite les sources retenues et les éventuelles divergences constatées avec EGL3.

## 8. Tests et critères de réussite

Écris des tests de comportement déterministes avec des réponses HTTP et un flux WebSocket/STOMP contrôlés. Ils doivent couvrir le vrai chemin Rust et le pont Swift utile, sans compte réel ni délais arbitraires. Les fixtures de protocole doivent être vérifiées indépendamment de l'encodeur testé.

Cas à couvrir :

- Liste vide ou complète, pagination si applicable, filtrage des relations, doublons, ajouts, suppression et blocage.
- Présence initiale, présence reçue avant la liste, événements pendant une réponse REST et resynchronisation sans résurrection d'un ami supprimé.
- Champs absents, effacements explicites, valeurs inconnues, noms et avatars indisponibles, jeu inconnu.
- En ligne vers en jeu, jeu A vers jeu B, fermeture du jeu, hors ligne, plusieurs namespaces ou connexions, activité masquée et retrait des données périmées.
- Décodage STOMP, heartbeats, message optionnel inconnu, erreur de protocole, bornes mémoire et fin de connexion.
- Révision sans changement, arrivée d'un événement au moment de la souscription, annulation pendant une attente et observateur lent.
- Reconnexion, resynchronisation, réveil, refus d'accès, authentification rejetée et arrêt des tentatives. Vérifier l'absence de polling et de boucle active.
- Rotation des identifiants pendant le service social, y compris si l'opération suivante échoue ou si aucun événement de présence ne suit. Vérifier la sauvegarde, le mode sans mémorisation et le changement de compte pendant l'écriture.
- Une reconnexion sociale ne révoque pas OAuth et n'annule pas un téléchargement. Le renouvellement partagé et la déconnexion volontaire conservent leur comportement attendu.
- Fermeture et réouverture de modale sans abonnement supplémentaire, deux magasins connectés simultanément, déconnexion d'un seul magasin, anciennes réponses et anciens avatars après changement de compte.
- Compteur indépendant de la recherche, états de connexion, routage de la modale, accessibilité et absence de régression Steam.

Exécute les tests ciblés pendant le travail, puis `./scripts/test.sh` pour la validation finale prévue par le dépôt. Ce script couvre notamment la localisation, le formatage Rust, Clippy, les tests Rust et macOS, ainsi que la signature. Rapporte les vérifications réellement exécutées, les échecs et les éventuels tests ignorés.

Vérifie visuellement la modale Steam et Epic dans l'application, notamment à la taille minimale actuelle de 860 × 600, en clair et sombre, avec recherche, noms longs, contenu défilant et navigation clavier. Réutilise le style existant au lieu de le redessiner.

La validation réelle doit distinguer la liste reçue, la présence initiale et un changement spontané observé. Si l'accès au compte ou un ami témoin manque, indique les scénarios restant à vérifier. Les tests locaux ne prouvent pas à eux seuls la livraison par Epic.

La fonctionnalité est terminée lorsque les vrais amis Epic s'affichent dans la modale commune, leurs statuts se mettent à jour par événements, le service survit aux fermetures de panneau et aux reprises réseau prévues, les comptes restent isolés et Steam fonctionne toujours. Toute limite externe non résolue doit apparaître explicitement dans le bilan.

## Résultat à fournir

Livre le code intégré et les tests, puis un compte rendu concis : comportement obtenu, fichiers principaux, transport effectivement retenu, vérifications exécutées, preuve ou absence de preuve des mises à jour réelles et limites restantes. Va jusqu'à l'intégration vérifiable, sans t'arrêter à un plan ou à une maquette.
