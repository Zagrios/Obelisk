# Compatibilité SDK Steamworks dans GameNative

Inspection des sources publiques le 21 septembre 2026. GameNative est épinglé à [`f1e7a7354f2a5d5d302cbe3233e571b87a380536`](https://github.com/utkarshdalal/GameNative/tree/f1e7a7354f2a5d5d302cbe3233e571b87a380536), Proton amont à [`5b89db940e0ebe3a137a6009a3589232fe084c09`](https://github.com/ValveSoftware/Proton/tree/5b89db940e0ebe3a137a6009a3589232fe084c09). Aucun jeu, bootstrap ou binaire téléchargé n'a été exécuté.

## Ce que fait le mode Bionic Steam

GameNative restaure les `steam_api.dll` d'origine pour ses modes Real Steam et Bionic Steam. Son autre branche peut remplacer ces DLL ou `steamclient.dll`. Il faut distinguer ces chemins lorsqu'on compare la compatibilité. [Choix du mode](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/ui/model/MainViewModel.kt#L636-L648), [restauration](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/utils/SteamUtils.kt#L897-L946).

Le mode Bionic installe le client Android `libsteamclient.so` et un pont `lsteamclient` assorti à la version de Proton choisie. Il configure les chemins de chargement, l'identité Steam, l'App ID et les points de communication. [Dépendances](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/utils/launchdependencies/BionicSteamAssetsDependency.kt#L19-L55), [environnement](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/com/winlator/xenvironment/components/BionicProgramLauncherComponent.java#L533-L599).

`SteamBootstrap.prepareApp` démarre un processus séparé avec trois arguments visibles : App ID, chemin du client Steam natif, fichier de statut. Il attend ensuite un état READY, FAILED ou CLOSED. Il ne reçoit pas le chemin du SDK du jeu. Certains commentaires Java décrivent encore un chargement dans le processus Android ; le code Kotlin actuel utilise bien `ProcessBuilder`. [Démarrage et attente](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/SteamBootstrap.kt#L121-L168).

La recherche dans `app/src/main` ne trouve pas `SteamAPI_InitFlat`, `SteamAPI_Init` ou `SteamUser_v0`. Le chemin public inspecté prépare le moteur et son pont, sans précontrôle visible exigeant InitFlat ou User_v023 dans la DLL du jeu. C'est une conclusion limitée au code public. L'implémentation de `libsteambootstrap.so` est explicitement absente du dépôt et propriétaire. [Notice du bootstrap](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/THIRD_PARTY_NOTICES#L105-L126).

## Où Proton adapte les versions

Proton possède une table de constructeurs distincts pour `SteamUser021`, `SteamUser022` et `SteamUser023`, ainsi que de nombreuses versions antérieures. Il sélectionne le constructeur avec le nom demandé. [Table des versions](https://github.com/ValveSoftware/Proton/blob/5b89db940e0ebe3a137a6009a3589232fe084c09/lsteamclient/steamclient_generated.c#L200-L214), [sélection](https://github.com/ValveSoftware/Proton/blob/5b89db940e0ebe3a137a6009a3589232fe084c09/lsteamclient/steamclient_main.c#L278-L325).

Le chemin `GetISteamUser` transmet la chaîne de version au client natif, puis construit l'interface Windows correspondante. Il ne remplace donc pas systématiquement une demande User021 par User023. [Appel côté Windows](https://github.com/ValveSoftware/Proton/blob/5b89db940e0ebe3a137a6009a3589232fe084c09/lsteamclient/winISteamClient.c#L6683-L6695), [appel côté Unix](https://github.com/ValveSoftware/Proton/blob/5b89db940e0ebe3a137a6009a3589232fe084c09/lsteamclient/cppISteamClient_SteamClient020.cpp#L98-L103).

Cela démontre la stratégie d'adaptation de Proton amont. Cette inspection ne compare pas les archives Proton distribuées par GameNative à ce commit, et ne prouve pas une réussite réelle de Thronefall sur Android ou macOS.

## Autre mode et conséquence pour Forge

La branche qui remplace `steam_api.dll` extrait d'abord les chaînes de versions présentes dans la DLL et écrit `steam_interfaces.txt`. C'est une autre stratégie, distincte du chemin Bionic qui conserve le SDK du jeu. [Extraction des versions](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/utils/SteamUtils.kt#L214-L249), [remplacement](https://github.com/utkarshdalal/GameNative/blob/f1e7a7354f2a5d5d302cbe3233e571b87a380536/app/src/main/java/app/gamenative/utils/SteamUtils.kt#L300-L324).

L'enseignement pour Forge est de respecter la version offerte par le SDK chargé. Ces sources ne montrent pas qu'ajouter uniquement `Init` suffirait à un vérificateur qui impose ensuite User_v023. Elles ne justifient pas non plus une refonte de tout le runtime : l'initialisation du SDK et la sélection de ses interfaces sont deux choix séparés. Cette dernière conclusion est une recommandation d'architecture, pas un résultat d'exécution.
