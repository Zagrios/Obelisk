# Implémenter les amis Steam en temps réel dans Forge

Travaille dans `/Users/mathieu/Documents/Github/Zagrios/forge`. Implémente cette fonctionnalité dans le code, avec ses tests et sa vérification visuelle. Les choix produit ci-dessous sont validés ; avance sans redemander leur confirmation. Réponds en français.

## Objectif et périmètre

Afficher les vrais amis du compte Steam connecté, leurs avatars, leur présence et leur jeu en cours dans une modale centrée dans la fenêtre de Forge. Les changements doivent arriver par les événements de la connexion Steam, sans interrogation périodique des statuts.

La présentation doit être réutilisable pour Epic et GOG plus tard. Cette tâche implémente seulement Steam. Les invitations à jouer, les demandes d'amitié, le chat, le lancement des jeux Windows, les succès et la synchronisation cloud feront l'objet de tâches distinctes. Préserve une structure qui pourra les accueillir, sans boutons factices ni intégrations anticipées.

L'utilisateur veut à terme une seule application à installer. Cette première fonctionnalité doit utiliser la session Steam native de Forge, sans installer Steam, Wine, CrossOver, GameNative ou un SDK propriétaire. Les amis et la présence ne nécessitent pas la passerelle Steamworks d'un jeu.

## 1. Examiner l'existant avant les modifications

Lis les instructions du dépôt et vérifie l'état Git. Préserve tous les fichiers et changements préexistants, y compris les fichiers non suivis. Ne fais aucun commit, push ou publication.

Les observations suivantes ont été vérifiées le 16 septembre 2026 ; confirme-les dans le checkout actuel :

- SwiftUI/AppKit gèrent l'interface, Rust les protocoles, avec un pont UniFFI.
- Le menu du compte contient actuellement le nom, le nombre de jeux, l'actualisation et la déconnexion. Il est fourni par `SteamView` à `StorefrontTitleBar`, partagé avec Epic.
- Le client Steam possède déjà une connexion CM en WebSocket TLS. Une tâche Tokio reçoit les messages et maintient la connexion pendant les téléchargements HTTP.
- `ClientPersonaState` est déjà décodé partiellement pour le nom et l'avatar du compte connecté. Cela ne constitue pas encore une gestion de la liste des amis ni de leurs statuts.
- Les réponses aux requêtes de bibliothèque et de téléchargement ont leur propre circuit. Les nouveaux événements sociaux doivent coexister avec ces réponses.
- `SteamModel` est détenu par `AppModel` et possède déjà des protections par génération contre les réponses d'un ancien compte.

Fichiers à examiner, tous sous `/Users/mathieu/Documents/Github/Zagrios/forge/` :

| Partie | Chemins relatifs à cette racine |
| --- | --- |
| Connexion et messages Steam | `crates/storefronts/steam-kit/src/cm.rs`, `crates/storefronts/steam-kit/src/proto.rs`, `crates/storefronts/steam-kit/src/client.rs` |
| Pont Rust/Swift | `crates/forge-bridge/src/steam.rs`, `crates/forge-bridge/src/lib.rs`, `uniffi.toml` |
| Session et présentation Steam | `apps/macos/Forge/Models/SteamModel.swift`, `apps/macos/Forge/Features/Steam/SteamView.swift` |
| Fenêtre et durée de vie | `apps/macos/Forge/App/AppModel.swift`, `apps/macos/Forge/Features/RootView.swift` |
| Style partagé | `apps/macos/Forge/DesignSystem/StorefrontTitleBar.swift`, `StorefrontPage.swift`, `StorefrontScrollView.swift`, `ForgeStyle.swift` dans le même dossier |
| Traductions | `apps/macos/Forge/App/L10n.swift`, `apps/macos/Forge/Resources/Localizable.xcstrings` |
| Tests existants | `apps/macos/ForgeTests/SteamIntegrationTests.swift`, `SteamPresentationTests.swift`, `SteamSortingPerformanceTests.swift` dans le même dossier, et tests Rust du kit |
| Documentation et scripts | `README.md`, `docs/architecture.md`, `docs/steam-cm-notes.md`, `scripts/test.sh`, `scripts/build-rust.sh`, `scripts/generate-project.sh` |

Examine aussi les protocoles de session et les doubles de test générés ou utilisés par ces fichiers. Garde les modifications centrées sur cette fonctionnalité.

## 2. Présentation validée

Dans le menu du compte Steam, ajoute une entrée localisée du type **Amis · 4 en ligne**. Le compteur correspond aux amis connectés, y compris ceux en jeu, et reste indépendant de la recherche et de l'ouverture de la modale. Pendant le chargement initial ou une coupure, affiche un état approprié plutôt qu'un faux zéro fiable.

Un clic ouvre une modale :

- Centrée dans toute la fenêtre de Forge, pas seulement dans la colonne Steam ou sous l'avatar ; aucune fenêtre système indépendante.
- Largeur cible autour de 520 points, hauteur limitée par la fenêtre, contenu défilant. Vérifier au minimum la fenêtre actuelle de 860 × 600.
- En-tête avec logo Steam, titre **Amis Steam**, compteur de connectés et bouton de fermeture.
- Recherche locale par pseudo, avec les conventions de recherche existantes pour la casse et les accents.
- Ligne d'ami avec avatar, pseudo, statut lisible et nom du jeu lorsqu'il est connu. Une couleur seule ne suffit pas pour indiquer le statut.
- Amis en jeu en premier, puis autres connectés ; ordre alphabétique stable à l'intérieur des groupes. Les hors ligne sont dans une section repliable. La recherche doit pouvoir les trouver, même si cette section était repliée.
- Repli propre pour avatar absent, nom manquant, nom long ou jeu dont le nom n'est pas disponible. Ne suppose pas que l'utilisateur possède le jeu de son ami pour pouvoir en afficher le nom.
- Fermeture par la croix ou Échap, gestion correcte du focus clavier et restitution du focus après fermeture. Le contenu derrière la modale ne doit pas recevoir les clics ou le focus.
- Respect du style existant : matériaux, couleurs, arrondis, typographie, thèmes clair/sombre, réduction des animations et de la transparence, accessibilité et libellés VoiceOver.

Distingue visuellement le chargement, une liste vide, aucun résultat de recherche, aucun ami connecté, une erreur et une connexion interrompue. Pendant une coupure, les dernières données peuvent rester visibles en étant indiquées comme non actualisées. Ne transforme pas tous les amis en hors ligne.

Pour cette étape, aucune action d'invitation, d'ajout d'ami, de discussion ou de lancement ne doit être présentée comme fonctionnelle.

## 3. Réutilisation pour Epic et GOG

Conçois une vue commune, par exemple `FriendsPanel`, et une ligne commune, par exemple `FriendRow`. Les noms et le découpage exacts restent à choisir selon l'existant.

Leur interface reçoit des données de présentation indépendantes des types protobuf et des types Steam générés. Prévois au minimum une identité de magasin, une identité de compte, un identifiant d'ami stable, son nom, son avatar, sa présence et son activité de jeu éventuelle. L'identité doit éviter toute collision entre magasins ou comptes.

La présence et le jeu en cours sont deux informations distinctes. Un ami peut être absent tout en ayant un jeu lancé. Préserve cette information dans le modèle et rends l'affichage cohérent. Distingue aussi présence inconnue et état global de connexion.

Steam convertit ses données vers cette présentation commune. Epic et GOG pourront fournir leurs propres données ultérieurement, sans dupliquer la modale. Évite un modèle social universel surdimensionné, des branches Steam dans chaque ligne ou de faux fournisseurs Epic/GOG. Un faux fournisseur destiné aux tests est approprié.

## 4. Implémentation Steam pilotée par événements

Étudie les sources de protocole ci-dessous avant d'ajouter des structures ou constantes. Vérifie les identifiants de messages, les numéros de champs protobuf, les valeurs d'énumération, les groupes de champs et les informations demandées à la connexion. Ne les invente pas.

Le déroulement attendu est :

1. À l'établissement d'une session Steam valide, recevoir ou obtenir l'état initial des relations d'amitié et demander les informations nécessaires par lots bornés si le protocole le requiert.
2. Recevoir les événements `ClientFriendsList` et `ClientPersonaState` sur la connexion CM existante.
3. Réduire ces événements vers un état des amis cohérent, associé au compte et à la session.
4. Publier les changements via le pont Rust/Swift et actualiser SwiftUI sur le MainActor.

Utilise une véritable observation : abonnement avec callback, flux adapté au pont, ou méthode asynchrone suspendue jusqu'au prochain changement. Une attente asynchrone réveillée par un événement convient. Une boucle avec temporisation appelant régulièrement `friends()` ou relisant un cache Rust depuis Swift ne convient pas.

Le chargement initial, un rattrapage ciblé et une resynchronisation après reconnexion sont permis. Aucun timer ne doit rafraîchir périodiquement la liste ou les statuts. Les heartbeats existants et les temporisations de reconnexion ont un autre rôle. Le polling du processus d'authentification Steam et celui de la progression des téléchargements sont hors de ce changement.

Cas de protocole à traiter et tester :

- Une liste complète remplace correctement l'état des relations, y compris lorsqu'elle est vide ; une mise à jour incrémentale ajoute, modifie ou retire seulement ce qu'elle contient. Hydrate aussi les informations des amis hors ligne : les changements spontanés ne suffisent pas à constituer leur état initial.
- Afficher uniquement les véritables amis. Écarter le compte lui-même, les clans, comptes bloqués et relations de demande en attente. Une demande acceptée ou un ami supprimé ailleurs doit néanmoins modifier la liste par événement.
- Une mise à jour de présence peut être partielle. L'absence d'un groupe de champs ne doit pas effacer le nom, l'avatar ou l'activité précédents. Une valeur vide ou nulle explicitement transmise dans le bon groupe doit pouvoir effacer l'ancienne donnée, notamment lors de la fermeture d'un jeu.
- Un passage explicite hors ligne doit faire disparaître l'activité de jeu devenue périmée, même si le paquet ne contient que la présence. Une reconnexion de cet ami ne doit pas faire réapparaître l'ancien jeu sans nouvelle information d'activité. Préserve les distinctions occupé, absent et inactif lorsqu'elles sont fournies.
- Gérer les valeurs Steam connues et les valeurs inconnues sans erreur fatale. Respecter la visibilité reçue du serveur et le réglage de présence du compte ; ne pas forcer le compte utilisateur en ligne ni prétendre détecter des amis invisibles.
- Gérer les informations de personne reçues avant la liste des relations, les mises à jour répétées et les suppressions. Éviter qu'une ancienne mise à jour de personne ne réintroduise un ami supprimé.
- Éviter de perdre un changement entre l'état initial et l'installation de l'observation. Une révision d'état ou une souscription atomique peut convenir.

Les informations sociales doivent disposer d'un routage séparé des réponses attendues par les opérations Steam existantes. Étends le lecteur existant ; aucun second lecteur ne doit consommer la même socket. Un paquet social optionnel malformé doit être traité sans casser les téléchargements, en conservant les validations de sécurité du transport. Le lecteur WebSocket ne doit pas attendre l'interface, une image, une requête HTTP ou un verrou détenu par un téléchargement. Une attente d'événement ne doit pas conserver le mutex global du client CM.

## 5. Cycle de vie, reconnexion et coût

La session du compte possède l'observation des amis. Fermer la modale ou passer à une autre section de Forge ne ferme pas cette observation. Réouvrir la modale réutilise l'état en mémoire, sans créer une seconde connexion ou un second abonnement réseau.

À la déconnexion volontaire ou au changement de compte, annule les observations et tentatives de reconnexion, purge les données sociales du compte précédent et ferme sa modale. Associe tous les résultats et événements à une génération de session. Une réponse tardive, y compris une image, ne doit pas réafficher des données de l'ancien compte.

Sur perte réseau ou sortie de veille, indique que les données ne sont plus actualisées. Prévois une reconnexion bornée avec délais progressifs, puis une resynchronisation complète des relations et de leur présence. Traite séparément un incident réseau et une authentification rejetée ou révoquée ; ce dernier cas doit demander une nouvelle connexion, sans boucle de reconnexion infinie. Réutilise l'architecture existante et préserve les erreurs et annulations des opérations en cours.

Garde une seule session logique active par compte pour cet usage. Les tokens restent dans les emplacements déjà prévus, en mémoire et dans le Keychain selon les préférences. Les états sociaux, logs, captures et fixtures ne doivent contenir aucun secret. La liste peut rester en mémoire pour cette étape ; aucun stockage disque supplémentaire n'est nécessaire.

Borne la mémoire et le travail lors des rafales d'événements. Si tu regroupes des mises à jour, le dernier état exact doit être conservé. Un canal qui abandonne silencieusement des deltas n'est pas acceptable. Les listes longues doivent être affichées paresseusement ; les avatars sont mis en cache et ne sont pas retéléchargés à chaque changement de statut. Le tri, la recherche et le focus doivent rester stables pendant les mises à jour.

## 6. Localisation et génération

Tous les textes destinés à l'utilisateur passent par `L10n` et `Localizable.xcstrings`, avec anglais et français complets, pluriels et paramètres corrects. Le repli reste l'anglais pour une langue système non prise en charge. Les erreurs Rust affichées utilisent les conventions `backend.*` existantes. Les noms des amis et des jeux sont des données.

Régénère les bindings UniFFI et le projet Xcode avec les scripts existants lorsque tes modifications le nécessitent. Les fichiers de `Bridge/Generated` ne s'éditent pas manuellement. Préserve la configuration de signature.

## 7. Vérification et conditions de réussite

Écris des tests de comportement avec un flux d'événements contrôlé, sans dépendre d'un compte réel ni de délais arbitraires. Pour les champs du protocole, ajoute des fixtures vérifiées indépendamment de l'encodeur du projet : encoder puis décoder les mêmes mauvais numéros ne prouve pas la compatibilité Steam. Les tests doivent couvrir :

- Liste initiale complète, ajouts et suppressions incrémentaux, filtrage des relations, déduplication.
- Fusion des groupes partiels, effacement explicite de l'activité, arrivée désordonnée liste/persona, enums inconnues.
- Transition en ligne → en jeu → en ligne → hors ligne, compteur et regroupement corrects, avec fermeture explicite du jeu.
- Recherche, avatars manquants, absence de nom de jeu, listes vides et erreurs distinctes.
- Mises à jour reçues modale fermée, réouverture sans abonnement supplémentaire, changement de section sans arrêt du flux.
- Coupure, reconnexion et rattrapage sans doublons ; annulation pendant une attente suspendue.
- Compte A remplacé par B avec événements tardifs de A ; déconnexion pendant le chargement ou la reconnexion.
- Coexistence des événements sociaux avec des réponses bibliothèque/téléchargement, sans blocage ni consommation de la mauvaise réponse.
- Réception d'un événement factice via le pont jusqu'à la présentation, sans appel de rafraîchissement périodique. Un test uniquement du réducteur Rust ne suffit pas à valider ce trajet.

Vérifie le rendu et les interactions de la modale dans l'application : centrage dans toute la fenêtre, petite fenêtre, thèmes, défilement, focus, recherche et fermeture au clavier. Utilise les outils disponibles ; indique clairement ce qui a été observé et ce qui ne l'a pas été.

Exécute les tests ciblés pendant le développement, puis le script actuel `/Users/mathieu/Documents/Github/Zagrios/forge/scripts/test.sh`. Il vérifie actuellement la localisation, le formatage Rust, Clippy, les tests du workspace, les tests Xcode et la signature. Relis-le avant exécution. Après toute dernière modification, relance les vérifications concernées. Mets à jour la documentation technique existante en anglais pour décrire le comportement et les limites réellement implémentés.

Si une session Steam de développement est accessible par les mécanismes normaux de Forge, vérifie la liste réelle et les événements observables. Ne récupère pas les secrets par des moyens détournés et ne contacte aucun ami pour organiser un test. Si une transition réelle ne peut pas être provoquée ou observée, termine les tests contrôlés et signale précisément cette validation restante. Distingue compilation, tests simulés, vérification visuelle et observation réelle des serveurs Steam.

Le travail est terminé lorsque l'implémentation Steam est intégrée, la modale réutilisable fonctionne, les données se mettent à jour par événements, les protections de session sont testées et les vérifications disponibles ont été exécutées. Fais un bilan concis des changements, tests et limites réelles, avec liens vers les fichiers concernés.

## Sources à consulter

Les dépôts communautaires ci-dessous sont des sources primaires de leurs propres implémentations ; ils ne constituent pas une garantie de support officiel de Forge par Valve. Vérifie la version consultée et conserve les références utiles dans la documentation du changement.

- **Traitement des listes d'amis et des présences Steam** : lire notamment `HandleFriendsList`, `RequestFriendInfo` et `HandlePersonaState` dans [SteamKit / SteamFriends.cs](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamFriends/SteamFriends.cs). C'est la référence principale pour le flux initial, les changements de relations et les groupes partiels. Forge garde son implémentation Rust ; aucun exécutable .NET n'est requis.
- **Contenu des notifications** : [SteamKit / Callbacks.cs](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamFriends/Callbacks.cs), notamment `FriendsListCallback` et `PersonaStateCallback`.
- **Schémas protobuf** : [SteamTracking / steammessages_clientserver_friends.proto](https://github.com/SteamTracking/Protobufs/blob/master/steam/steammessages_clientserver_friends.proto). Rechercher aussi les définitions d'énumérations et d'identifiants de messages dans les sources SteamKit/SteamTracking avant de coder.
- **Sémantique officielle des amis et de leur présence** : [Valve / ISteamFriends](https://partner.steamgames.com/doc/api/isteamfriends). Cette documentation explique le SDK des jeux ; elle ne doit pas être confondue avec une API HTTP pour launcher ni ajoutée comme dépendance à Forge.
- **État actuel du protocole Forge** : [notes CM locales](/Users/mathieu/Documents/Github/Zagrios/forge/docs/steam-cm-notes.md) et [architecture locale](/Users/mathieu/Documents/Github/Zagrios/forge/docs/architecture.md).
- **Contexte futur uniquement** : [recherche services des magasins](/Users/mathieu/Documents/Github/Zagrios/forge/docs/research-store-services.md). Elle distingue les services de launcher de ceux attendus par un jeu. Les recherches Wine/GameNative ne sont pas nécessaires pour implémenter cette première liste d'amis.

Commence par inspecter le code et ces sources, annonce brièvement le découpage retenu, puis réalise l'implémentation et ses vérifications. Ce prompt autorise le travail local décrit ; aucune nouvelle validation des choix produit n'est nécessaire.
