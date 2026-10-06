# Algorithme Expérimental de Calcul des Gains

## But

Ce document sert de base de discussion pour un nouvel algorithme expérimental de
calcul des gains dans Omniphony.

L'objectif n'est pas encore de figer une implémentation, mais de cadrer:

- le problème exact à résoudre
- les invariants qu'il faut préserver
- les hypothèses de calcul acceptables en temps réel
- les critères d'évaluation avant intégration dans le renderer principal

Le but principal de cette expérimentation est le suivant:

- obtenir un rendu plus réaliste des intrusions d'objets à l'intérieur du volume
  interne de la pièce

Le point de départ est que le rendu VBAP actuel se comporte principalement comme
un rendu projeté sur une sphère virtuelle autour de l'auditeur ou de la zone de
référence. Ce comportement fonctionne pour beaucoup de cas de spatialisation
périphérique, mais devient moins convaincant quand on veut rendre:

- des objets qui pénètrent réellement dans le volume de la salle
- des trajectoires passant "dans" l'espace écouté plutôt que sur une enveloppe
- des situations où la distance doit être rendue de manière plus crédible

Le système actuel dispose déjà de mécanismes palliatifs pour la distance, mais
ils sont considérés ici comme:

- partiellement fonctionnels
- utiles en pratique
- insuffisants pour produire un rendu vraiment convaincant ou "bluffant"

L'ambition du nouvel algorithme est donc double:

- mieux rendre les objets situés à l'intérieur du volume de la pièce
- permettre à terme l'utilisation d'enceintes placées à l'intérieur du volume,
  et pas uniquement en périphérie

Autrement dit, cette expérimentation ne cherche pas seulement une nouvelle loi
de gains plus élégante. Elle cherche un changement de modèle de rendu, passant
d'une logique surtout adaptée à une scène périphérique vers une logique capable
de mieux exploiter un volume de diffusion interne.

## Contexte

Aujourd'hui, Omniphony s'appuie principalement sur un pipeline de spatialisation
où les gains de sortie doivent rester:

- stables quand la source se déplace
- cohérents perceptuellement entre layouts
- économes en calcul dans les chemins chauds
- robustes aux cas limites géométriques

Un nouvel algorithme peut être pertinent si l'on veut améliorer au moins un de
ces points:

- continuité spatiale
- gestion du spread
- conservation d'énergie ou de loudness perçue
- comportement près des frontières entre triplets / régions
- compatibilité avec des layouts irréguliers
- réduction des artefacts de pompage ou de bascule

Mais ce n'est pas la motivation principale de cette note.

La motivation principale est plus spécifique:

- VBAP rend bien une logique de panning sur enveloppe périphérique
- VBAP rend moins naturellement l'occupation d'un volume intérieur
- les correctifs actuels de distance améliorent partiellement le résultat sans
  changer ce modèle de fond

Le nouvel algorithme est donc envisagé comme une piste pour sortir d'une logique
de "surface virtuelle" et aller vers une logique plus volumique.

## Problème Ciblé

Le problème à résoudre n'est pas simplement "calculer autrement les gains".

Le problème visé est plus précisément:

- représenter de manière plus crédible un objet qui se rapproche, traverse, ou
  occupe le volume intérieur de la pièce
- éviter que cet objet reste perçu comme projeté sur une enveloppe ou une
  sphère virtuelle
- ouvrir le modèle à des configurations où certaines enceintes sont elles-mêmes
  à l'intérieur du volume et non seulement sur la périphérie

Ce point change la lecture de toute l'expérimentation:

- si l'objectif était seulement d'améliorer le panning latéral sur une couronne
  périphérique, VBAP et ses dérivés resteraient une base naturelle
- ici, on cherche au contraire un modèle qui prenne plus au sérieux la
  dimension volumique de la scène

## Attentes Réalistes

Si l'expérimentation réussit, le gain attendu n'est pas forcément:

- une meilleure précision absolue sur tous les cas de panning périphérique

Le gain attendu est plutôt:

- une sensation plus crédible d'intrusion ou de présence interne d'un objet
- une meilleure cohérence entre position volumique cible et distribution réelle
  des gains
- un comportement plus naturel quand le layout contient ou contiendra des
  enceintes internes

Le bon critère de succès n'est donc pas seulement:

- "est-ce plus lisse que VBAP ?"

Mais aussi:

- "est-ce que l'objet cesse d'être perçu comme accroché à une sphère virtuelle
  ?"
- "est-ce que le rendu de proximité et de pénétration dans la pièce devient plus
  crédible ?"
- "est-ce que le modèle reste cohérent quand des enceintes ne sont plus toutes
  sur l'enveloppe extérieure ?"

## Hypothèse de Travail Actuelle

L'idée candidate est la suivante:

- on calcule un poids pour chaque enceinte à partir de sa distance à la position
  théorique de l'objet
- on transforme ces poids en gains
- on normalise ensuite l'ensemble pour retrouver l'énergie équivalente à une
  seule enceinte

Autrement dit, chaque enceinte contribue en fonction de sa proximité géométrique
avec l'objet visé, mais le niveau global reste borné par une normalisation
finale.

Cette direction a plusieurs avantages potentiels:

- formulation intuitive
- continuité naturelle quand l'objet se déplace
- comportement applicable à des layouts irréguliers
- cadre simple pour comparer différentes lois de décroissance
- compatibilité naturelle avec des enceintes situées dans le volume intérieur
- modèle plus directement lié à une notion de proximité volumique réelle

Le point critique est la définition exacte de:

- la métrique de distance
- la loi de pondération
- la normalisation énergétique
- l'éventuelle limitation du nombre d'enceintes actives

Décisions provisoires issues de la discussion:

- la distance visée est euclidienne
- cette distance doit être calculée dans l'espace transformé par la room
  geometry, pas dans un espace naïf ignorant les transformations de salle
- la loi `f(d)` reste ouverte à ce stade
- la stratégie d'activation des enceintes doit comparer au moins:
  - une variante "toutes les enceintes"
  - une variante "sélection dynamique par proximité jusqu'à erreur acceptable"

## Formulation de Base

Soit:

- `p` la position théorique de l'objet
- `s_i` la position de l'enceinte `i`
- `T_room(...)` la transformation liée à la room geometry
- `p' = T_room(p)`
- `s_i' = T_room(s_i)`
- `d_i = ||p' - s_i'||_2`

Une première forme candidate est:

- `w_i = f(d_i)`
- `g_i = w_i / sqrt(sum_j(w_j^2))`

Avec cette normalisation, on obtient:

- `sum_i(g_i^2) = 1`

Ce critère correspond bien à l'objectif "énergie équivalente à une seule
enceinte" si l'on prend comme référence une enceinte unique jouée à gain unité.

Remarque importante:

- si l'on normalise par `sum_i(w_i)` on conserve une somme linéaire
- si l'on normalise par `sqrt(sum_i(w_i^2))` on conserve une énergie
  quadratique

Dans votre idée, la seconde option semble être la bonne cible.

## Lois de Pondération à Comparer

La vraie liberté de design est surtout dans `f(d)`.

Quelques candidates simples:

### A. Inverse de distance

- `w_i = 1 / max(d_i, eps)`

Avantage:

- très simple

Risque:

- domination trop forte de l'enceinte la plus proche
- sensibilité élevée quand `d_i` devient très petit

### B. Inverse de distance au carré

- `w_i = 1 / max(d_i, eps)^2`

Avantage:

- séparation plus nette

Risque:

- comportement potentiellement trop agressif
- transitions moins douces

### C. Noyau borné et lisse

Exemples:

- `w_i = 1 / (d_i + eps)^alpha`
- `w_i = exp(-k * d_i^2)`
- `w_i = max(0, 1 - d_i / r)^alpha`

Avantage:

- plus facile à régler
- permet d'éviter certaines singularités

Risque:

- plus de paramètres à calibrer

## Point de Vigilance Géométrique

Dans cette piste, la distance retenue est la distance euclidienne.

Le point important n'est donc plus le type de distance, mais l'espace dans
lequel cette distance est mesurée.

Hypothèse de travail:

- on applique d'abord les transformations liées à la room geometry
- on calcule ensuite les distances euclidiennes dans cet espace transformé

Autrement dit, la proximité pertinente n'est pas seulement celle du layout brut,
mais celle du layout effectivement déformé ou reparamétré par la géométrie de
salle.

Conséquences à discuter:

- quelles transformations de room geometry entrent réellement dans `T_room`
- si `T_room` s'applique de manière strictement identique aux objets et aux
  enceintes
- si cette transformation doit être recalculée en temps réel ou pré-calculée
  partiellement

## Questions à Clarifier

Avant de choisir une formule, il faut verrouiller le besoin.

### 1. Quel problème veut-on corriger ?

Exemples possibles:

- rendu encore trop "sur sphère virtuelle" pour les objets proches ou internes
- rendu de distance insuffisamment crédible malgré les correctifs existants
- difficulté à exploiter proprement des enceintes situées dans le volume
- trop de variation de niveau quand un objet traverse certaines zones
- transitions trop abruptes entre ensembles de haut-parleurs
- mauvaise tenue avec des layouts asymétriques ou déformés par la room geometry
- spread peu satisfaisant perceptuellement
- compromis insuffisant entre précision de localisation et stabilité
- comportement indésirable de la loi distance -> gain actuelle ou absente

### 2. Quel comportement veut-on préserver ?

Points à expliciter:

- somme linéaire constante
- somme quadratique constante
- conservation d'énergie moyenne
- priorité à la précision de direction
- priorité à la stabilité temporelle
- priorité à la compatibilité avec le comportement actuel
- extinction stricte ou non des enceintes lointaines

### 3. Quel est le périmètre de l'expérimentation ?

Le prototype peut viser:

- uniquement les objets ponctuels
- uniquement un mode "experimental"
- seulement certains layouts
- seulement le calcul des gains de base
- ou aussi l'interaction avec le spread et les trims

Dans le cadre de cette note, le périmètre prioritaire est:

- les cas où l'objet pénètre le volume interne de la pièce
- les cas où la distance et la proximité doivent être perçues plus fortement
- les layouts présents ou futurs incluant des enceintes non strictement
  périphériques

## Contraintes Non Négociables

### Temps réel

L'algorithme doit éviter dans le chemin chaud:

- allocations par frame ou par bloc
- recherche dynamique coûteuse dans des structures générales
- branches complexes dépendant fortement des cas limites
- recomputations inutiles de quantités géométriques stables

Conséquence pratique:

- pré-calculer tout ce qui peut l'être au chargement du layout
- garder un nombre borné de sorties actives par objet si possible
- privilégier des opérations vectorielles simples et prédictibles
- éviter une normalisation qui oblige à traiter toutes les enceintes si une
  version sparse donne le même résultat perceptif

### Robustesse

Le calcul doit rester bien défini quand:

- la source est très proche d'un axe ou d'une frontière
- plusieurs haut-parleurs ont des géométries quasi dégénérées
- le layout est incomplet ou irrégulier
- les coordonnées d'entrée approchent les bornes de l'espace de travail
- une enceinte est pratiquement confondue avec la position cible

### Intégration

Le mode expérimental doit idéalement:

- coexister avec le mode actuel
- être activable explicitement
- produire des métriques comparables
- permettre un rollback simple

## Invariants Audio à Tester

Chaque proposition devrait être évaluée contre les invariants suivants:

- pas de saut audible lors d'un déplacement continu
- pas d'explosion de gain local
- niveau global borné et prédictible
- somme quadratique des gains égale ou très proche de `1`
- image spatiale stable sur trajectoires lentes
- comportement monotone quand une source s'approche clairement d'un haut-parleur
- comportement raisonnable quand plusieurs sorties sont presque équidistantes

## Variantes d'Implémentation

Même en gardant votre principe de base, plusieurs variantes sont possibles:

### A. Toutes les enceintes contribuent

Principe:

- calculer un poids pour toutes les enceintes
- normaliser ensuite globalement

Intérêt:

- formulation la plus simple
- champ de gains très continu

Risque:

- image potentiellement trop diffuse
- coût proportionnel au nombre total d'enceintes

### B. Seuil dynamique par proximité et erreur cible

Principe:

- trier les enceintes par proximité croissante
- ajouter les enceintes une à une
- recalculer les gains normalisés sur l'ensemble courant
- estimer la position résultante produite par ces gains
- s'arrêter quand cette position résultante est suffisamment proche de la cible

Intérêt:

- critère de sélection piloté par le résultat spatial, pas par un seuil fixe
- nombre d'enceintes actives potentiellement réduit
- adaptable à des layouts irréguliers

Risque:

- il faut définir une métrique fiable de "position résultante"
- coût supplémentaire si l'on réévalue trop souvent l'erreur
- risque de transition visible si l'ensemble actif change brutalement

### C. Voisinage local + lissage

Principe:

- sélectionner un voisinage restreint
- lisser l'entrée/sortie des enceintes dans l'ensemble actif

Intérêt:

- bon compromis entre compacité et continuité

Risque:

- un peu plus de logique d'état ou d'hystérésis

### D. Pré-calcul partiel

Principe:

- pré-calculer une partie de la relation position -> voisinage ou poids
- garder une normalisation finale analytique

Intérêt:

- moins de coût dans le hot path

Risque:

- mémoire et complexité de génération

### E. Variante mixte

Principe:

- utiliser le seuil dynamique par proximité comme logique de sélection
- appliquer ensuite un lissage pour éviter les bascules de set actif

Intérêt:

- conserve le critère géométrique fort
- réduit le risque d'artefacts au passage d'un ensemble d'enceintes à un autre

Risque:

- plus de paramètres
- comportement plus difficile à lire si la logique n'est pas instrumentée

## Reconstruction de Position pour le Seuil Dynamique

L'idée de base du seuil dynamique est:

- ajouter des enceintes par ordre de proximité
- calculer les gains correspondants
- reconstruire la position effectivement "visée" par cet ensemble de gains
- arrêter l'ajout quand cette position reconstruite est suffisamment proche de
  la cible

Cette logique a du sens seulement si la reconstruction est cohérente avec le
modèle direct utilisé pour calculer les gains.

Point important:

- il ne faut pas forcément chercher une inverse exacte du mapping
  `position -> gains normalisés`
- après normalisation énergétique, une partie de l'échelle absolue est perdue
- dans beaucoup de cas, on cherchera donc une reconstruction compatible plutôt
  qu'une inversion stricte

## Pourquoi l'inverse exacte est difficile

Le modèle direct est de la forme:

- `d_i = ||T_room(p) - T_room(s_i)||_2`
- `w_i = f(d_i)`
- `g_i = w_i / sqrt(sum_j(w_j^2))`

La normalisation couple tous les gains entre eux.

Conséquences:

- un gain donné `g_i` ne dépend pas seulement de `d_i`
- il dépend aussi de tous les autres poids `w_j`
- on ne peut donc pas, en général, reconstruire chaque distance `d_i` de manière
  indépendante à partir du seul `g_i`

Autrement dit:

- une inverse exacte de `g_i -> d_i` n'existe pas forcément sous une forme
  simple
- la reconstruction de position devra souvent être une pseudo-inverse ou une
  minimisation d'erreur

## Options de Reconstruction

### A. Pseudo-inverse analytique approximative

Principe:

- ignorer temporairement une partie de l'effet de normalisation
- utiliser une fonction compatible avec `f` pour estimer des proximités ou des
  distances relatives

Exemple:

- si `w_i = 1 / d_i^alpha`, on peut utiliser une grandeur du type
  `d_i_rel ~ 1 / g_i^(1 / alpha)` ou `1 / w_i^(1 / alpha)` selon la variable
  retenue

Puis:

- reconstruire une position approchée à partir de ces distances implicites

Intérêt:

- simple
- peu coûteux

Risque:

- approximation parfois grossière
- fortement dépendante de la loi `f(d)`

### B. Barycentre pondéré cohérent avec le modèle

Principe:

- utiliser les gains ou les poids pour reconstruire une position résultante par
  barycentre dans l'espace transformé

Exemples:

- `p_rec = sum_i(g_i * s_i') / sum_i(g_i)`
- `p_rec = sum_i(g_i^2 * s_i') / sum_i(g_i^2)`
- `p_rec = sum_i(w_i * s_i') / sum_i(w_i)`

Intérêt:

- très simple
- facile à instrumenter
- probablement suffisant comme premier critère d'arrêt

Risque:

- ce n'est pas une vraie inverse du modèle de gains
- le barycentre peut être biaisé, surtout si la loi `f(d)` est non linéaire

### C. Reconstruction par minimisation d'erreur sur les gains

Principe:

- chercher la position `p*` qui reproduit au mieux les gains observés
- autrement dit, résoudre:
- `p* = argmin_p error(g_obs, g_theorique(p))`

où:

- `g_obs` est le vecteur de gains obtenu avec l'ensemble actif courant
- `g_theorique(p)` est le vecteur de gains prédit par le modèle direct pour une
  position candidate `p`

Exemples d'erreur possibles:

- erreur quadratique sur les gains
- erreur quadratique sur les gains au carré
- erreur pondérée privilégiant les enceintes dominantes

Intérêt:

- très cohérent avec le modèle direct
- ne suppose pas d'inverse analytique artificielle
- reste valable quelle que soit la loi `f(d)`

Risque:

- plus coûteux
- nécessite une stratégie d'optimisation ou une approximation tabulée

### D. Reconstruction hybride

Principe:

- utiliser d'abord un estimateur simple de type barycentre
- raffiner seulement si nécessaire avec une minimisation locale

Intérêt:

- bon compromis coût / cohérence

Risque:

- plus de complexité de contrôle

## Recommandation Provisoire

Pour une première expérimentation, le plus pragmatique semble être:

1. définir une reconstruction simple par barycentre dans l'espace transformé
2. mesurer si ce critère suffit pour piloter le seuil dynamique
3. si le critère est trop biaisé, passer à une reconstruction par minimisation
   d'erreur

Cela permet de valider rapidement l'idée du seuil dynamique sans imposer trop
tôt une machinerie d'inversion complexe.

## Critique Avertie de l'Algorithme

Cette section ne part pas du principe que l'idée est mauvaise. Elle vise plutôt
à clarifier ce que l'algorithme peut raisonnablement apporter, et ce qu'il ne
garantit pas à lui seul.

## Ce que l'algorithme a pour lui

Sur le plan structurel, l'approche a plusieurs qualités réelles:

- elle est simple à formuler
- elle est relativement simple à instrumenter
- elle favorise naturellement la continuité quand l'objet se déplace
- elle s'adapte plus facilement qu'un schéma trop rigide à des layouts
  irréguliers ou transformés par la room geometry
- elle permet un compromis explicite entre compacité spatiale et stabilité
  via la loi `f(d)` et la stratégie de sélection des enceintes

Comme algorithme expérimental, c'est donc une base sérieuse.

## Ce qu'on peut en attendre sur le rendu sonore

Si le choix de `f(d)` et de la stratégie de sélection est bon, on peut espérer:

- des transitions plus douces que dans un schéma à bascule franche entre
  ensembles d'enceintes
- une meilleure stabilité des gains le long de trajectoires continues
- un comportement moins fragile sur des géométries de salle non idéales
- un niveau global plus régulier si la contrainte énergétique est bien tenue

En revanche, il ne faut pas attendre automatiquement:

- une localisation perceptive optimale
- une image fantôme naturellement précise
- une largeur de source bien contrôlée dans tous les cas
- une constance de loudness perçue simplement parce que l'énergie est
  normalisée

## Limite fondamentale: géométrie n'est pas perception

Le principal point faible théorique de l'approche est le suivant:

- la proximité géométrique entre un objet cible et les enceintes n'est pas un
  proxy suffisant de la localisation auditive perçue

Un calcul de gains basé sur la distance euclidienne peut être mathématiquement
régulier, tout en produisant:

- une source trop large
- une direction apparente biaisée
- une image centrale molle
- un rendu trop dépendant de la densité locale d'enceintes

Autrement dit, l'approche peut être très bonne pour produire un champ de gains
continu, sans être nécessairement bonne pour produire une scène sonore précise.

## Limites perceptives attendues

### 1. Risque d'image trop diffuse

Si trop d'enceintes restent actives avec des gains non négligeables:

- la source peut perdre son focus
- on obtient une masse sonore étalée au lieu d'un objet net

Ce risque est particulièrement élevé:

- si `f(d)` décroît trop lentement
- si le mode "toutes les enceintes" est utilisé brut
- si le critère du seuil dynamique accepte trop facilement des ensembles larges

### 2. Risque inverse: comportement trop agressif

Si `f(d)` décroît trop vite:

- l'enceinte la plus proche domine trop tôt
- les transitions deviennent localement raides
- on réintroduit un comportement proche d'une bascule dure, mais masquée sous
  une formule continue

### 3. Normalisation d'énergie != loudness perçue constante

La contrainte:

- `sum_i(g_i^2) = 1`

est utile pour borner le niveau énergétique, mais elle ne garantit pas:

- une égalité de niveau perçu entre une seule enceinte et plusieurs enceintes
- un comportement subjectif constant selon la bande fréquentielle
- une constance de loudness quand la corrélation entre sorties change

En pratique, un rendu énergétiquement propre peut rester perceptivement trop
fort, trop large, ou au contraire trop flou.

### 4. Dépendance très forte à la loi `f(d)`

Le coeur réel du rendu n'est pas simplement "distance puis normalisation".

Il dépend surtout de:

- la forme exacte de `f(d)`
- sa pente près des faibles distances
- son comportement asymptotique sur les enceintes lointaines
- l'existence ou non d'un rayon d'influence pratique

Sans un bon choix de `f(d)`, le principe général ne suffit pas à produire un
bon panner.

### 5. Dépendance forte à la définition de `T_room`

L'intégration de la room geometry est logique, mais elle ajoute une difficulté:

- si `T_room` capture une correction utile, elle améliore le modèle
- si `T_room` déforme seulement l'espace de calcul sans lien assez fort avec la
  perception, elle peut introduire des voisinages "mathématiquement proches"
  mais perceptivement discutables

Le risque est donc de rendre l'algorithme plus cohérent avec un espace interne
de calcul, mais pas forcément avec l'image sonore réellement entendue.

## Problèmes d'implémentation attendus

### 1. Le mode "toutes les enceintes" est simple mais coûteux et diffus

Avantages:

- facile à coder
- très continu

Inconvénients:

- coût proportionnel au nombre total d'enceintes
- tendance naturelle à élargir la source

### 2. Le seuil dynamique est prometteur mais fragile

Cette variante est probablement la plus intéressante, mais elle pose plusieurs
problèmes:

- le critère d'arrêt dépend entièrement de la définition de la position
  résultante reconstruite
- une faible erreur géométrique ne garantit pas une faible erreur perceptive
- l'ensemble actif peut changer fréquemment près de certaines frontières

Sans précautions, cette variante peut recréer exactement ce qu'elle cherche à
éviter:

- micro-bascule des enceintes actives
- respiration de la largeur apparente
- modulation subtile du timbre ou de l'image spatiale

### 3. Hystérésis probablement nécessaire

Si une enceinte entre ou sort de l'ensemble actif dès qu'un seuil est franchi,
il y a un fort risque de discontinuité.

Il faudra probablement prévoir:

- une hystérésis sur le critère d'ajout/retrait
- un nombre minimal et maximal d'enceintes actives
- éventuellement un lissage temporel sur les gains ou sur le set actif

### 4. La reconstruction de position est sous-déterminée

Pour piloter le seuil dynamique, il faut comparer:

- la position cible
- la position "reconstruite" à partir des gains

Mais cette position reconstruite n'est pas une grandeur évidente. Selon la
méthode retenue:

- barycentre simple
- barycentre énergétique
- pseudo-inverse
- minimisation d'erreur

on peut obtenir des décisions d'activation assez différentes.

Le comportement du seuil dynamique dépendra donc fortement de cette définition.

## Conclusion critique

En tant qu'algorithme expérimental:

- l'idée est crédible
- elle mérite clairement un prototype
- elle est bien adaptée à une phase d'exploration instrumentée

En tant qu'algorithme de rendu final:

- elle n'est pas encore convaincante par principe
- elle devra prouver qu'elle ne gagne pas seulement en continuité mathématique,
  mais aussi en qualité perceptive

Et plus précisément, elle devra prouver qu'elle améliore réellement:

- la sensation d'objet présent dans le volume interne
- le rendu des intrusions et proximités dans la pièce
- la cohérence d'un système de diffusion qui ne serait plus limité à une
  couronne ou coque périphérique

La conclusion la plus prudente est donc:

- bonne base pour explorer stabilité et robustesse géométrique
- base encore faible si l'objectif principal est la précision perceptive de
  l'image sonore

Mais ce jugement doit être lu à la lumière du but réel de l'expérimentation:

- si le but premier est de mieux rendre une scène volumique interne plutôt que
  d'optimiser seulement un panner périphérique classique, alors cette approche a
  une motivation plus forte qu'un simple remplacement esthétique de VBAP

Le succès réel dépendra probablement de trois éléments plus que du principe
général lui-même:

- la loi `f(d)`
- la logique de sélection des enceintes
- la définition de la reconstruction utilisée pour le seuil dynamique

## Où VBAP reste probablement meilleur

Il faut expliciter un point important pour éviter un faux débat:

- si l'objectif est un panning périphérique classique sur une enveloppe
  d'enceintes externes, VBAP garde probablement des avantages structurels

Cas où VBAP a de bonnes chances de rester supérieur:

- objets perçus principalement sur une couronne ou sphère périphérique
- recherche d'une image fantôme compacte entre un petit nombre d'enceintes
- cas où la direction apparente compte plus que la présence volumique interne
- layouts conçus précisément autour d'une géométrie de panning de type VBAP

Pourquoi:

- VBAP est naturellement orienté vers une interpolation directionnelle sur une
  enveloppe
- il limite souvent mieux le nombre d'enceintes dominantes
- il peut produire une image plus nette quand le problème est essentiellement
  angularisé

Autrement dit, si la scène visée reste fondamentalement "sur la surface", le
nouvel algorithme n'a pas de raison évidente d'être meilleur par défaut.

## Où le nouvel algorithme a une vraie chance d'être supérieur

L'approche distance + normalisation devient plus intéressante quand on s'écarte
justement du cas "surface virtuelle".

Cas où elle peut avoir un avantage structurel:

- objets qui s'approchent fortement de l'auditeur ou traversent le volume
  intérieur
- scènes où la proximité physique relative aux enceintes devient un signal utile
- layouts irréguliers où une logique purement périphérique devient artificielle
- configurations futures avec enceintes situées à l'intérieur du volume
- cas où l'on veut que la distribution des gains reflète une présence dans le
  volume plutôt qu'une simple direction sur l'enveloppe

Pourquoi:

- le modèle repose directement sur une proximité volumique
- il n'impose pas que toutes les enceintes pertinentes vivent sur une coque
  extérieure
- il peut théoriquement intégrer plus naturellement des topologies de diffusion
  internes

Le nouvel algorithme n'est donc pas seulement une variante de panner. Il
pourrait devenir plus pertinent dès que le système de diffusion n'est plus
conceptuellement réduit à une enveloppe externe.

## Lecture comparative correcte

La bonne lecture n'est probablement pas:

- "remplacer VBAP partout"

Mais plutôt:

- conserver l'idée que VBAP reste une référence forte pour le panning
  périphérique
- tester si le nouvel algorithme devient meilleur précisément quand l'objet
  entre dans le volume interne ou quand le layout cesse d'être purement
  périphérique

Vu sous cet angle, l'expérimentation devient plus défendable:

- si elle échoue sur les cas VBAP classiques, ce n'est pas forcément un échec
  du concept
- elle réussit seulement si elle apporte un vrai gain sur les cas où VBAP est
  structurellement moins naturel

Le benchmark pertinent ne doit donc pas demander:

- "est-ce meilleur partout ?"

Mais plutôt:

- "est-ce meilleur là où le modèle VBAP montre ses limites de sphère virtuelle
  ?"

## Critique par Cas d'Écoute

Cette section anticipe les comportements plausibles de l'algorithme selon
plusieurs situations d'écoute typiques.

## Source frontale centrée

Cas:

- un objet cible est placé au centre frontal
- plusieurs enceintes frontales peuvent être géométriquement proches

Ce qu'on peut espérer:

- un rendu stable
- peu de saut de gains si l'objet bouge légèrement autour du centre

Ce qui peut mal se passer:

- l'image centrale devient trop large
- le centre fantôme manque de précision
- les enceintes latérales proches reçoivent une contribution excessive

Pourquoi:

- la proximité géométrique ne contraint pas assez fortement la compacité de
  l'image
- un centre perceptivement convaincant demande souvent un équilibre plus
  directionnel que simplement métrique

Lecture comparative:

- sur ce cas, VBAP a de bonnes chances de rester meilleur si l'objectif est
  simplement un centre fantôme périphérique net
- le nouvel algorithme devient intéressant seulement si l'on cherche un objet
  réellement perçu comme entrant dans le volume interne plutôt qu'un simple
  point frontal sur enveloppe

## Source entre deux enceintes voisines

Cas:

- la cible se trouve dans une zone intermédiaire entre deux enceintes

Ce qu'on peut espérer:

- une transition très douce entre les deux enceintes dominantes
- moins de rupture qu'avec une sélection dure

Ce qui peut mal se passer:

- si `f(d)` est trop douce, des enceintes secondaires polluent l'image
- si `f(d)` est trop raide, on retrouve une quasi-bascule

Point critique:

- c'est probablement l'un des cas où l'algorithme a le plus de chances de bien
  fonctionner, à condition que la loi `f(d)` soit bien choisie

Lecture comparative:

- si la scène reste purement périphérique, VBAP peut rester plus compact et plus
  prévisible
- si la scène vise déjà une interpolation plus volumique, l'écart peut devenir
  moins net

## Source latérale

Cas:

- la cible se situe sur un côté du champ sonore

Ce qu'on peut espérer:

- une répartition localement cohérente sur les enceintes latérales proches

Ce qui peut mal se passer:

- le rendu se diffuse trop vers l'avant ou l'arrière
- la latéralité apparente perd en netteté

Pourquoi:

- plusieurs enceintes peuvent être euclidiennement proches sans être
  perceptivement équivalentes pour la latéralisation

Lecture comparative:

- avantage probable à VBAP si le besoin principal est une latéralisation nette
  sur l'enveloppe
- avantage possible au nouvel algorithme si la scène latérale doit aussi rendre
  une profondeur ou une intrusion dans le volume

## Source arrière

Cas:

- l'objet se situe derrière l'auditeur

Ce qu'on peut espérer:

- une certaine robustesse sur des layouts arrière irréguliers

Ce qui peut mal se passer:

- ambiguïté avant / arrière insuffisamment traitée
- participation excessive d'enceintes non arrière si elles restent proches dans
  l'espace transformé

Pourquoi:

- la perception front/back est fragile
- une métrique euclidienne peut manquer d'information pour stabiliser une image
  arrière convaincante

Lecture comparative:

- aucun avantage automatique pour le nouvel algorithme ici
- il faudra démontrer qu'il n'aggrave pas les ambiguïtés arrière déjà délicates

## Source en hauteur

Cas:

- l'objet est placé nettement au-dessus ou en dessous du plan principal

Ce qu'on peut espérer:

- sur un layout dense, une interpolation continue entre couches d'enceintes

Ce qui peut mal se passer:

- hauteur perçue trop faible
- mélange excessif entre couche principale et couche haute
- source qui semble "élargie" verticalement au lieu d'être bien placée

Pourquoi:

- la proximité spatiale brute ne correspond pas forcément aux indices de
  hauteur perçus

Lecture comparative:

- sans modèle perceptif supplémentaire, le nouvel algorithme n'a pas d'avantage
  évident sur ce cas
- son intérêt réapparaît surtout si des enceintes hautes ou internes forment un
  vrai volume de diffusion plutôt qu'une simple couche externe

## Layout irrégulier ou asymétrique

Cas:

- certaines zones ont plus d'enceintes que d'autres
- la room geometry déforme encore la relation spatiale

Ce qu'on peut espérer:

- meilleur comportement que des méthodes reposant sur une structure locale
  fragile
- adaptation naturelle aux géométries non idéales

Ce qui peut mal se passer:

- biais systématique vers les zones plus denses
- largeur apparente variable selon la région du layout
- rendu non homogène selon la direction visée

Pourquoi:

- une zone dense offre mécaniquement plus de contributeurs proches
- après normalisation, cela peut changer la texture spatiale même si l'énergie
  reste bornée

Lecture comparative:

- c'est un des cas où le nouvel algorithme peut réellement avoir quelque chose à
  apporter
- dès que la géométrie s'éloigne d'une enveloppe régulière, l'approche
  volumique devient conceptuellement plus pertinente

## Mouvement continu

Cas:

- l'objet se déplace lentement ou rapidement dans l'espace

Ce qu'on peut espérer:

- grande continuité des gains
- disparition de certaines ruptures typiques des changements de région

Ce qui peut mal se passer:

- respiration de la largeur de source
- variation du nombre d'enceintes actives
- micro-instabilités quand le seuil dynamique ajoute ou retire une enceinte

Point critique:

- même si les gains individuels sont continus, la largeur apparente et la
  position reconstruite peuvent ne pas l'être autant

Lecture comparative:

- VBAP peut rester meilleur sur des trajectoires périphériques bien couvertes
- le nouvel algorithme peut devenir meilleur si la trajectoire traverse
  réellement le volume au lieu de longer une surface virtuelle

## Passage près d'une enceinte

Cas:

- la cible passe très près d'une enceinte réelle

Ce qu'on peut espérer:

- une domination intuitive de cette enceinte

Ce qui peut mal se passer:

- gain local excessif avant normalisation
- domination trop brutale si `f(d)` diverge trop vite
- rendu qui "colle" à l'enceinte plus que souhaité

Conséquence pratique:

- il faudra presque certainement un `eps` ou une loi bornée près de `d = 0`

Lecture comparative:

- c'est un cas central pour la nouvelle approche
- si elle n'est pas meilleure ici que les palliatifs de distance actuels, elle
  rate probablement sa cible principale

## Lecture d'ensemble

L'algorithme semble naturellement fort dans les cas où l'on cherche:

- continuité
- robustesse géométrique
- comportement lisse sur déplacements

Il semble naturellement plus fragile dans les cas où l'on cherche avant tout:

- netteté de l'image fantôme
- précision perceptive stricte
- homogénéité de rendu entre régions très différentes du layout

La vraie valeur de cette approche dépendra donc surtout de la réponse à cette
question:

- veut-on d'abord un panner géométriquement stable, ou un panner
  psychoacoustiquement plus contraint ?

Et, plus concrètement encore:

- veut-on optimiser un rendu périphérique déjà bien servi par VBAP
- ou veut-on traiter les cas où un objet doit réellement exister à l'intérieur
  du volume de la pièce ?

## Comparaison avec VBAP et Cavern

Cette section sert à situer l'algorithme envisagé par rapport à deux références
utiles:

- le rendu VBAP actuellement utilisé dans Omniphony
- les moteurs de rendu présents dans Cavern

## VBAP actuel dans Omniphony

Lecture simplifiée du comportement visé:

- logique principalement orientée vers une enveloppe d'enceintes périphériques
- rendu très naturel pour des objets pensés comme projetés sur une surface
  virtuelle
- moins naturel pour des objets censés pénétrer réellement le volume interne

Points forts probables de VBAP:

- bonne logique directionnelle
- image souvent compacte
- comportement bien adapté à des enceintes disposées sur une périphérie
- bon point de départ pour un panning "sur coque" ou "sur sphère virtuelle"

Limites par rapport au but de cette expérimentation:

- le modèle de base reste très lié à une logique d'enveloppe
- la distance est traitée par des correctifs additionnels plutôt que par un
  modèle volumique fondamental
- l'utilisation d'enceintes internes n'est pas son terrain naturel

## Cavern: lecture rapide des modes pertinents

Le code de Cavern montre qu'il ne repose pas sur un seul moteur.

Il distingue notamment:

- des layouts symétriques avec moteur "balance-based"
- des layouts asymétriques avec moteur directionnel ou hybride
- un traitement de la distance séparé via rolloff, et en casque via
  virtualisation dédiée

Cette architecture est utile comme point de comparaison, car elle montre une
tentative de sortir du pur panning périphérique sans pour autant adopter votre
algorithme.

## Cavern en layout symétrique

Dans le mode symétrique, Cavern:

- place la source dans un espace lié à la taille de l'environnement
- cherche une boîte englobante de haut-parleurs
- interpole entre couches et coins de cette structure
- peut ajouter une extension de taille vers davantage d'enceintes

Ce mode est intéressant parce qu'il est déjà plus volumique qu'un simple rendu
sur sphère.

Par rapport à votre algorithme:

- il partage l'idée de rendre un objet dans un volume
- mais il dépend d'une structure géométrique régulière ou semi-régulière
- il ne repose pas sur une loi générale `distance -> poids` appliquée à toutes
  les enceintes

Conséquence:

- Cavern symétrique est plus structuré que votre approche
- votre approche est potentiellement plus générale pour des layouts irréguliers
  ou avec enceintes internes

## Cavern en layout asymétrique

Dans le mode asymétrique, Cavern:

- calcule des correspondances angulaires entre la direction de la source et les
  enceintes
- utilise une pondération très fortement directionnelle
- réduit souvent le mix aux quelques enceintes les plus pertinentes
- normalise ensuite le résultat

Par rapport à votre algorithme:

- c'est une logique clairement orientée enveloppe et direction
- la proximité volumique n'est pas le critère principal
- la compacité de l'image est mieux protégée
- la capacité à rendre une intrusion dans le volume n'est pas son axe principal

Conséquence:

- Cavern asymétrique est plus proche d'une extension du paradigme VBAP que de
  votre proposition

## Distance dans Cavern

Il faut aussi noter que Cavern traite déjà la distance, mais surtout de façon
séparée du panning principal:

- atténuation par distance via un rolloff scalaire
- traitement spécifique de distance pour la virtualisation casque

Ce point est important pour la comparaison:

- Cavern ne semble pas faire du rendu volumique multi-enceintes interne par une
  loi générale de proximité à toutes les enceintes
- la distance y est surtout un modulateur du signal ou du rendu binaural, pas
  le coeur du moteur de répartition multi-enceintes

## Position de l'algorithme envisagé

L'algorithme en discussion pour Omniphony se situe donc à un endroit différent:

- plus volumique dans son principe que VBAP
- moins contraint géométriquement que le mode symétrique de Cavern
- moins directionnel et moins "surface-oriented" que le mode asymétrique de
  Cavern

Il peut être vu comme une troisième voie:

- adaptée à des objets dans le volume
- potentiellement compatible avec des enceintes internes
- potentiellement compatible avec des layouts irréguliers

Mais cette généralité a un prix:

- risque plus fort de diffusion excessive
- risque plus fort de perte de compacité de l'image
- besoin plus fort d'une bonne loi `f(d)` et d'une bonne logique de sélection

## Ce que Cavern apporte à la réflexion

Le code de Cavern confirme plusieurs intuitions utiles:

- il existe un vrai besoin de distinguer rendu périphérique et rendu plus
  volumique
- un moteur de rendu peut chercher à modéliser un volume sans se limiter à une
  sphère virtuelle
- dès qu'on veut garder une image nette, une forme de structure locale ou de
  sparsification réapparaît presque toujours

Autrement dit, Cavern renforce deux idées à la fois:

- votre intuition volumique est pertinente
- mais un modèle purement distance-based devra être discipliné pour ne pas
  devenir trop diffus

## Résumé comparatif

VBAP:

- naturellement fort pour le panning périphérique
- faible pour les objets réellement internes au volume

Cavern symétrique:

- déjà plus volumique
- mais dépend d'une structure régulière de type boîte / couches / coins

Cavern asymétrique:

- très orienté direction et compacité
- moins naturellement adapté à une présence interne dans le volume

Nouvel algorithme Omniphony:

- orienté proximité volumique
- ouvert aux enceintes internes et aux layouts irréguliers
- prometteur pour sortir du paradigme de la sphère virtuelle
- fragile si la loi `f(d)` et la sélection dynamique ne contrôlent pas assez la
  diffusion

## Proposition de Cadre d'Expérimentation

Pour éviter une discussion trop abstraite, l'expérimentation devrait produire
les mêmes artefacts comparables à chaque itération:

### Entrées de test

- quelques layouts de référence
- un ensemble de trajectoires d'objets déterministes
- plusieurs valeurs de spread
- quelques cas pathologiques proches des frontières

### Sorties à comparer

- vecteur de gains par frame ou par échantillon de trajectoire
- somme des gains
- somme quadratique des gains
- nombre de sorties actives
- dérivée temporelle des gains
- cartes d'erreur ou de discontinuité
- erreur de direction perçue estimée si une métrique exploitable est définie
- erreur entre position cible et position résultante reconstruite

### Écoute et validation

- écoute A/B contre l'algorithme actuel
- écoute A/B entre plusieurs lois `f(d)`
- analyse sur cas statiques
- analyse sur mouvements lents
- analyse sur mouvements rapides
- vérification des pics de niveau
- vérification du comportement quand l'objet est très proche d'une enceinte
- vérification du comportement quand le seuil dynamique ajoute ou retire une
  enceinte

## Critères de Décision

Un algo expérimental mérite d'aller plus loin s'il apporte clairement au moins
un bénéfice mesurable sans dégrader les contraintes de base.

Critères possibles:

- amélioration audible sur les transitions
- métriques de continuité meilleures que l'existant
- coût CPU compatible avec la cible temps réel
- complexité d'implémentation et de maintenance acceptable
- comportement compréhensible et réglable
- respect robuste de la contrainte `sum_i(g_i^2) = 1`
- erreur de position reconstruite suffisamment faible avec un nombre
  d'enceintes actives raisonnable

## Questions Ouvertes

- Comment définir exactement `T_room` dans le calcul des distances ?
- Quelle loi `f(d)` donne le meilleur compromis stabilité / précision ?
- Comment définir la position résultante utilisée par le seuil dynamique ?
- Quel seuil d'erreur est acceptable avant d'ajouter une enceinte de plus ?
- Le spread doit-il modifier les distances, les poids, ou arriver après la
  normalisation ?
- Quels layouts doivent servir de vérité terrain pour l'évaluation ?
- Quel degré de compatibilité avec le rendu actuel est requis ?

## Prochaine Étape Recommandée

Avant toute implémentation large, il serait utile de définir:

1. un énoncé très court du défaut actuel observé
2. la définition opérationnelle de `T_room`
3. une ou deux lois `f(d)` à comparer
4. la définition de la position résultante pour le seuil dynamique
5. un protocole de comparaison reproductible

Une fois ces points fixés, ce document peut évoluer vers une note de design
plus précise avec pseudo-code, métriques cibles et plan d'intégration.
