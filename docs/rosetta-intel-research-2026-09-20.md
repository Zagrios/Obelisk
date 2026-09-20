# Rosetta et lancement Intel dans Forge

Recherche du 20 septembre 2026, préparatoire à l'implémentation du handoff `forge-rosetta-intel-handoff-2026-09-20.md`. Les sources externes ci-dessous proviennent d'Apple et de Valve. Aucun jeu ni auxiliaire `.app` Intel n'a été ouvert pendant cette recherche. Rosetta n'a pas été installé. Ce document ne prouve donc pas encore le parcours système complet.

## État local revérifié

Les contrôles de la session principale ont établi les faits suivants le 20 septembre 2026 :

- Le Mac exécute macOS 27.0, build `26A428`, avec `uname -m` égal à `arm64`.
- `/usr/bin/arch -x86_64 /usr/bin/true` échoue avec le code 1 et le message `arch: posix_spawnp: /usr/bin/true: Bad CPU type in executable`.
- `pkgutil --pkg-info com.apple.pkg.RosettaUpdateAuto` ne trouve aucun reçu.
- Le dépôt était propre au début de la session, sur `main`, à `78f6b64`, avec deux commits d'avance. La mention des nombreux changements non commités dans le handoff est périmée.
- Le contrôle ciblé `cargo test --locked -p steam-runtime --lib` a terminé avec 85 tests réussis, aucun échec et un test ignoré. Il ne remplace ni `scripts/test.sh`, ni un build Intel, ni les validations système et jeu.

Le test d'exécution prouve l'indisponibilité actuelle du chemin Intel testé. L'absence du reçu la corrobore. Un reçu seul ne serait pas une preuve de fonctionnement. Cette session n'a pas établi si l'absence actuelle résulte précisément de la mise à jour de macOS.

## Ce qu'Apple documente

Apple décrit l'ouverture d'une application Intel comme le parcours permettant de demander l'installation de Rosetta. L'installation peut demander une connexion Internet et les identifiants macOS. Si une application Intel s'ouvre effectivement sur un Mac avec puce Apple, Apple considère que Rosetta est installé et fonctionne. Le document ne garantit pas le comportement d'une ouverture directe d'un exécutable Mach-O isolé. [Assistance Apple, publiée le 14 septembre 2026](https://support.apple.com/fr-fr/102527).

Une application universelle utilise de préférence sa partie `arm64`. Rosetta traduit un processus entier, y compris ses bibliothèques chargées dynamiquement. Un même processus ne peut pas mélanger du code `arm64` et `x86_64`. La compilation Intel des auxiliaires et de l'adaptateur doit donc accompagner les bibliothèques Intel du jeu. Cette contrainte ne règle pas la compatibilité avec un ancien SDK Steam. [Environnement de traduction Rosetta](https://developer.apple.com/documentation/apple-silicon/about-the-rosetta-translation-environment).

`sysctl.proc_translated` indique si le processus appelant est traduit. L'exemple Apple renvoie 0 pour un processus natif, 1 pour un processus traduit et -1 pour une erreur. Une réponse 0 dans Forge natif ne dit pas si Rosetta est installé et ne distingue pas à elle seule un Mac Intel d'un Mac avec puce Apple. [Détection du processus traduit, WWDC20](https://developer.apple.com/videos/play/wwdc2020/10686/).

## Ouverture de l'auxiliaire et reprise

`NSWorkspace.openApplication(at:configuration:completionHandler:)` lance une application à partir de son URL. Son résultat distingue une application lancée d'une erreur de lancement. Le callback s'exécute sur une file concurrente. L'intégration Swift doit donc revenir au propriétaire de l'état de préparation avant de le modifier. [API NSWorkspace](https://developer.apple.com/documentation/appkit/nsworkspace/openapplication(at:configuration:completionhandler:)).

`NSWorkspace.OpenConfiguration.promptsUserIfNeeded` vaut `true` par défaut. Avec cette option, macOS peut afficher les erreurs, demandes d'authentification et autres interfaces nécessaires. Apple précise que les callbacks concernés attendent la fermeture de cette interface. Cette garantie générale rend l'approche du handoff plausible ; elle ne décrit pas les codes d'erreur propres à l'annulation de Rosetta ni chaque étape de son installation. [Option promptsUserIfNeeded](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration/promptsuserifneeded).

L'option `architecture` permet de demander une architecture précise ; sa valeur par défaut laisse macOS choisir celle qu'il préfère. Un auxiliaire exclusivement `x86_64` évite qu'une partie ARM native s'ouvre sans exercer Rosetta. Le choix d'utiliser cet auxiliaire comme déclencheur d'installation reste une déduction de conception à valider dans Forge. [Option architecture](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration/architecture).

La reprise proposée doit conserver l'admission existante et l'identité de la tentative. Après la fin du parcours Apple, Forge doit refaire un test d'exécution Intel contrôlé avant de poursuivre. Un callback ancien ne doit rien relancer après annulation, déconnexion ou remplacement de la tentative. Ces règles proviennent du parcours approuvé et des contraintes de Forge ; elles ne sont pas des garanties de LaunchServices.

Le premier test système devra établir l'affichage de l'invite avec l'auxiliaire réellement empaqueté, le résultat exact après annulation, le comportement en cas d'échec réseau et la reprise après installation. Il devra aussi vérifier qu'une fermeture rapide de l'auxiliaire ne crée pas une erreur de lancement et que le callback permet réellement d'observer la fin attendue. Si ce comportement ne suffit pas, il faudra un signal de réussite de l'auxiliaire lié à la tentative et une attente annulable. Aucun de ces comportements n'a été testé ici.

## Signature et empaquetage

Apple prévoit `Contents/Helpers` pour les applications et outils auxiliaires. Le code imbriqué doit être signé avant le bundle parent ; la signature extérieure enregistre celle du code imbriqué. Apple recommande la signature par étapes de l'intérieur vers l'extérieur et permet la vérification récursive avec `codesign --verify --deep --strict`. La signature valide ne prouve ni l'apparition de l'invite Rosetta, ni la réussite du lancement. [Apple TN2206, sections Nested Code et Using the codesign Tool's --deep Option Correctly](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

## Particularités de macOS 27 et limite de macOS 28

Les notes de macOS 27 indiquent qu'une ancienne installation de Rosetta n'est pas automatiquement restaurée après la mise à jour. Elles indiquent aussi que les applications précédemment configurées par l'utilisateur pour utiliser Rosetta sont lancées nativement. La présence passée et les préférences historiques ne suffisent donc pas pour autoriser le lancement actuel. [Notes de version macOS 27, section Rosetta](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).

Il faut éviter `VZLinuxRosettaDirectoryShare.availability` et `installRosetta()` pour tester ou installer Rosetta destiné aux applications macOS sur ce Mac. Apple documente qu'à partir de macOS 27, la disponibilité renvoie toujours `installed` et l'installation termine immédiatement. La traduction des applications Linux dans une machine virtuelle ARM est désormais intégrée au système. Ce résultat ne prouve donc pas qu'un binaire macOS Intel peut être exécuté. [Exécution de binaires Intel dans les machines virtuelles Linux](https://developer.apple.com/documentation/virtualization/running-intel-binaries-in-linux-vms).

Apple confirme que macOS 27 est la dernière version avec la prise en charge générale de Rosetta. Au-delà, Apple conserve un sous-ensemble destiné à certains jeux anciens non maintenus dépendant de frameworks Intel. Les sources consultées ne garantissent pas l'éligibilité de Dead Cells, de l'adaptateur Forge ou d'un auxiliaire Intel générique. Il ne faut donc pas annoncer ce parcours comme compatible avec macOS 28. [Annonce Apple du 1er septembre 2026](https://developer.apple.com/news/?id=w5ngl9k2).

Les notes de macOS 27 décrivent un mode de test des anciens jeux limité aux versions bêta, qui désactive Rosetta et peut casser les processus autres que les jeux. Il ne constitue pas une solution produit et n'a pas été activé. [Notes de version macOS 27, section Gaming](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).

## Périmètre d'implémentation retenu

Le parcours validé reste la vérification existante des fichiers, la synchronisation Cloud, puis la préparation Rosetta lorsque le lancement Intel l'exige. La modal et ses textes du handoff sont déjà approuvés. Le lancement doit ensuite conserver la propriété Rust du processus et du Cloud, l'admission Swift, l'annulation, les protections du compte et le superviseur existant.

La modal n'est qu'une partie du travail. Il faut également résoudre le choix d'architecture des auxiliaires, leurs dépendances et les appels compatibles avec l'ancien SDK Steam. Les jeux ARM et universels doivent garder leur chemin natif compatible. Aucun accès à l'installation du client Steam officiel n'est nécessaire à ce parcours.

Les documents Cloud du 20 septembre prévalent sur les anciens comptes rendus : [compatibilité actuelle](steam-cloud-compatibility-2026-09-20.md) et [prochaines validations](steam-cloud-next-validation.md). La persistance et les transferts SDK ordinaires sont implémentés ; `FileShare`, Dynamic Cloud, historique et migrations ne doivent pas être ajoutés dans ce travail Rosetta.

La préparation technique permet de commencer l'implémentation. Le déclenchement réel de l'installation, son annulation, la reprise et le vrai cycle Intel/ancien SDK/Cloud restent des preuves distinctes à obtenir avec l'accord demandé dans le handoff avant toute installation système ou ouverture du jeu.

## Audit technique du code et des binaires

Les observations suivantes portent sur le commit `78f6b648321c8fc63f4a4dbd02de51e107891a96` et les artefacts locaux inspectés. Aucun fichier produit n'a été modifié.

`file` confirme que l'exécutable Dead Cells installé par Forge est exclusivement `x86_64`, que sa bibliothèque Steam contient `i386` et `x86_64`, et que les deux auxiliaires actuellement embarqués par Forge sont `arm64`. L'inspection du runtime Valve préparé dans `build/steam-runtime/load-check` trouve les parties `x86_64` et `arm64` dans ses bibliothèques ; leurs dépendances Intel privées sont déjà présentes. Cela établit la disponibilité des binaires, pas leur fonctionnement ensemble.

Le [crate natif](../crates/steam-native/src/lib.rs) interdit la compilation hors ARM64 macOS. Son [exécutable](../crates/steam-native/src/bin/forge-steam-host.rs) refuse également tout processus traduit avant de sélectionner son mode. Le [module ABI](../crates/steam-native/src/abi.rs) contrôle des adresses et dispositions privées propres à la partie ARM du runtime épinglé. Une simple compilation du host en Intel ne suffit donc pas.

La route proposée conserve le superviseur et le moteur privé ARM64. Elle ajoute un consommateur SDK `x86_64` et un adaptateur universel, choisis selon les architectures compatibles de l'exécutable et de sa bibliothèque. Le [lanceur de consommateurs](../crates/steam-runtime/src/process.rs), le [paquet](../crates/steam-runtime/src/package.rs) et le [script de build](../scripts/build-steam-native.sh) doivent distinguer ces rôles. La cible Rust `x86_64-apple-darwin` n'est pas installée au moment de l'audit. L'IPC entre le consommateur Intel et le moteur ARM reste une hypothèse à valider par exécution.

Le [vérificateur SDK](../crates/steam-native/src/sdk.rs) exige actuellement `SteamAPI_InitFlat` et des accesseurs `SteamAPI_Steam*_v*` absents de la bibliothèque Dead Cells. Celle-ci exporte `SteamAPI_Init`, `SteamClient`, les handles utilisateur/pipe et des accesseurs `SteamAPI_ISteamClient_*`. Valve documente la récupération d'interfaces versionnées depuis `SteamClient`, avec les handles et une chaîne de version. Cela donne une route pour l'ancien SDK, sans prouver son ABI particulière. [Interface ISteamClient](https://partner.steamgames.com/doc/api/ISteamClient).

Les chaînes observées dans le `steam.hdll` installé par Forge sont `SteamClient017`, `SteamUser019`, `SteamUtils009`, `STEAMAPPS_INTERFACE_VERSION008`, `STEAMUSERSTATS_INTERFACE_VERSION011` et `STEAMREMOTESTORAGE_INTERFACE_VERSION014`. Les wrappers anciens doivent être associés à des versions vérifiées ; obtenir systématiquement `RemoteStorage016` ne suffit pas à établir la compatibilité de leurs appels. L'identité, la licence, les callbacks et le stockage doivent garder leurs contrôles réels.

## Intégration et validations à écrire

[SteamModel](../apps/macos/Forge/Models/SteamModel.swift) conserve la vérification existante selon le réglage utilisateur et le reçu. [SteamGameSessions](../apps/macos/Forge/Models/SteamGameSessions.swift) réserve le lancement avant les attentes. La synchronisation Cloud effective se déroule dans `run_game` du [superviseur Rust](../crates/steam-runtime/src/supervisor/server.rs), avant `prepare()` natif. C'est entre ces deux étapes que l'attente Rosetta doit se placer pour respecter l'ordre approuvé.

L'état et la réponse Rosetta doivent traverser le protocole du superviseur et UniFFI. La présentation peut s'attacher à [SteamView](../apps/macos/Forge/Features/Steam/SteamView.swift), où le modal Cloud est déjà attaché. Chaque réponse doit rester liée à la session, au compte et à la décision courante, comme le fait [SteamCloudModel](../apps/macos/Forge/Models/SteamCloudModel.swift). Rust revérifie le fonctionnement avant de poursuivre. La réservation reste active pendant toute l'attente.

L'annulation du téléchargement seule ne peut pas interrompre cette étape après Cloud. L'annulation de préparation et la révocation du compte doivent réveiller l'attente Rosetta, supprimer la reprise tardive et terminer sans afficher une erreur parasite. La perte de connexion au superviseur ne doit pas libérer prématurément une session incertaine.

Les tests doivent couvrir les architectures ARM, universelle et Intel, Rosetta fonctionnel ou indisponible, le double clic, l'annulation Forge et Apple, l'échec, la nouvelle tentative, la reprise et les callbacks devenus périmés après changement de compte. Les contrôles de build, signature et localisation restent distincts. La validation réelle doit ensuite établir l'initialisation de l'ancien SDK, l'IPC entre architectures, le lancement de Dead Cells et son cycle Cloud.
