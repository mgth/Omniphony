# Install Plan

## Objectif

Définir un scénario de référence de "première installation" pour `gsrd` et `SpatialVisualizer`, afin de garantir un démarrage fluide sans configuration préalable et de rendre les échecs compréhensibles pour l'utilisateur.

## Scénario de référence

### 1. Machine neuve pour le projet

- aucun fichier de config `gsrd`
- aucun fichier de config `SpatialVisualizer`
- aucun layout utilisateur
- aucun cache local pertinent

### 2. Binaires installés correctement

- `gsrd` présent
- bridge présent à l'emplacement packagé attendu
- `SpatialVisualizer` présent
- backend audio compilé pour la plateforme

### 3. Premier lancement `gsrd`

Sans argument autre que l'entrée si nécessaire, `gsrd` doit :

- trouver le bridge
- choisir un backend valide
- choisir un device par défaut
- choisir un layout par défaut sûr
- ouvrir OSC sans erreur
- démarrer sans nécessiter d'édition manuelle de config

### 4. Premier lancement `SpatialVisualizer`

Sans fichier `osc_config.json`, le visualizer doit :

- détecter `gsrd` sur les valeurs par défaut attendues
- s'enregistrer via OSC
- afficher un état clair si `gsrd` n'est pas encore là
- se synchroniser dès que `gsrd` arrive

### 5. Résultat attendu côté utilisateur

- aucun fichier à éditer
- aucun chemin à deviner
- aucun port à saisir
- aucun écran vide incompréhensible
- soit ça marche directement
- soit l'erreur explique exactement ce qui manque

## Critères de succès minimaux

- `gsrd` démarre sans config
- `SpatialVisualizer` démarre sans config
- les deux se voient automatiquement
- un layout par défaut apparaît
- les contrôles principaux sont utilisables
- en cas d'échec, le message est actionnable

## Constat actuel

### `gsrd`

- l'absence de config est tolérée, mais pas réellement "initialisée"
- le bridge n'a pas encore de bootstrap "first install" robuste
- les choix par défaut backend/device/layout existent, mais sont dispersés
- une config invalide peut retomber sur les defaults sans assez de contexte utilisateur

### `SpatialVisualizer`

- l'absence de config OSC est tolérée
- il n'y a pas encore de notion forte de "setup incomplet"
- en cas de problème de connexion/config, l'état utilisateur n'est pas encore assez explicite

## Axes de travail

1. Détecter explicitement le mode "first run".
2. Générer ou matérialiser une config minimale valide quand elle est absente.
3. Valider explicitement au démarrage :
   - bridge
   - backend audio
   - output device
   - OSC
   - layout
4. Exposer un état clair côté visualizer quand l'environnement n'est pas prêt.
5. Utiliser un panneau de logs / diagnostics pour rendre ces échecs lisibles par l'utilisateur.

## Stratégie de distribution

### Linux

Conclusion retenue :

- ne pas démarrer par Flatpak
- ne pas utiliser AppImage comme format principal d'installation
- définir d'abord un layout d'installation système stable
- utiliser Arch comme cible de test locale
- produire ensuite un `.deb` avec la même arborescence

#### Priorités Linux

1. créer un package Arch natif (`PKGBUILD`) pour pouvoir installer, désinstaller et upgrader localement sur la machine de dev
2. valider la structure d'installation réelle avec ce package
3. traduire ensuite cette structure en package `.deb`
4. garder AppImage uniquement comme format éventuel de preview manuelle

#### Pourquoi

- le projet installe plusieurs composants : `gsrd`, `SpatialVisualizer`, bridge, layouts, fichiers desktop, icônes, et potentiellement un service user
- un package Arch permet de tester rapidement les vrais chemins d'installation et les vrais comportements d'upgrade
- AppImage est utile pour lancer une app, mais mauvais comme premier test d'une installation système complète
- Flatpak ajoute des contraintes de sandbox et de permissions qui compliquent inutilement la première phase

#### Layout d'installation Linux visé

- `/usr/bin/gsrd`
- `/usr/bin/spatial-visualizer`
- `/usr/lib/spatial-renderer/libtruehd_bridge.so`
- `/usr/share/spatial-renderer/layouts/...`
- `/usr/share/applications/spatial-visualizer.desktop`
- `/usr/share/icons/hicolor/...`

#### Principes

- les configs utilisateur ne doivent pas être générées au moment de l'installation système
- les configs utilisateur doivent être générées ou matérialisées au premier lancement
- `gsrd` doit savoir résoudre le bridge et les layouts depuis les emplacements packagés
- un service `systemd --user` pourra être ajouté plus tard, mais ne doit pas bloquer le premier packaging

#### Ordre de travail recommandé

1. figer le layout d'installation Linux
2. rendre `gsrd` et `SpatialVisualizer` compatibles avec ce layout
3. produire un `PKGBUILD`
4. tester install / uninstall / upgrade sur Arch
5. produire ensuite le `.deb`

### Windows

Conclusion retenue :

- utiliser un installateur MSI comme format principal
- garder un `.zip` portable seulement pour le debug interne ou les builds de développement
- définir un layout d'installation Windows stable avant d'automatiser le packaging

#### Priorités Windows

1. figer l'arborescence installée sous `Program Files`
2. s'assurer que `gsrd` sait résoudre le bridge et les layouts depuis cette arborescence
3. produire un installeur MSI
4. tester install / uninstall / upgrade sur une machine Windows réelle
5. garder le `.zip` comme artefact secondaire, non principal

#### Pourquoi

- MSI est mieux adapté qu'un zip pour une installation complète avec désinstallation propre, upgrades, raccourcis et intégration système
- le projet installe plusieurs composants et pas seulement un exécutable unique
- cette approche reste compatible avec l'ajout futur d'un service Windows pour `gsrd`

#### Layout d'installation Windows visé

- `C:\Program Files\Spatial Renderer\gsrd\gsrd.exe`
- `C:\Program Files\Spatial Renderer\SpatialVisualizer\spatial-visualizer.exe`
- `C:\Program Files\Spatial Renderer\bridge\truehd_bridge.dll`
- `C:\Program Files\Spatial Renderer\layouts\...`

#### Configs utilisateur Windows

- `%APPDATA%\gsrd\config.yaml`
- `%APPDATA%\SpatialVisualizer\osc_config.json`

#### Principes

- les configs utilisateur ne doivent pas être écrites pendant l'installation MSI
- les configs utilisateur doivent être créées ou matérialisées au premier lancement
- le bridge et les layouts doivent être trouvables depuis les emplacements packagés sans saisie manuelle
- un service Windows pour `gsrd` pourra être ajouté plus tard, mais ne doit pas bloquer le premier packaging

#### Ordre de travail recommandé

1. figer le layout d'installation Windows
2. rendre `gsrd` et `SpatialVisualizer` compatibles avec ce layout
3. produire l'installeur MSI
4. tester install / uninstall / upgrade
5. générer éventuellement un zip portable en artefact secondaire

## Lien avec le panneau de logs

Le panneau de logs n'est pas seulement un outil de debug développeur.

Il servira aussi à :

- informer l'utilisateur qu'une config est absente
- signaler qu'un bridge est introuvable
- signaler qu'aucun backend audio valide n'est disponible
- signaler qu'aucun device de sortie n'a été résolu
- indiquer si l'enregistrement OSC avec `gsrd` a échoué
- guider la correction au premier lancement
