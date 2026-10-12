# WebGL ↔ compositeur : aliasing de textures sur WebKitGTK

Note interne sur une classe de bug récurrente dans Omniphony Studio sous Tauri Linux. À lire avant d'ajouter quoi que ce soit qui chevauche le canvas WebGL ou de proposer une solution dynamique de blur, glow, ou snapshot du viewport 3D.

> ## Resolution (2026-06, WebKitGTK 2.52.4) — backdrop blur re-enabled
>
> The overlay backdrop blur is back on the left/right panels and the expanded
> log overlay. Re-tested on WebKitGTK **2.52.4** (webkit2gtk-4.1): the broad
> triggers from the historical record (panel resize, visibility transitions) no
> longer reproduce on this version. The **one remaining trigger** was a native
> HTML5 drag accidentally started by an orbit-drag on the WebGL canvas — the OS
> drag ghost is a compositor snapshot, and dropping it aliases the sprite
> textures (the classic corruption).
>
> Fix / guard: a `dragstart` `preventDefault` scoped to `#omniphony-renderer-mount`
> (in `scene/setup.js`, on the persistent mount) plus `-webkit-user-drag: none`
> on the canvas. This blocks only native drag *originating in the 3D canvas* — the
> speaker list's own drag-to-reorder and all pointer/slider/text input are
> untouched. With that guard in place, native `backdrop-filter: blur()` is safe
> again. Defense-in-depth kept from the earlier mitigation: blur is still reset to
> `none` while a panel is collapsed or being resized.
>
> The historical analysis below stays as the decision record — and the hard rule
> still holds for **other** compositor-promoting properties (`filter`, extra
> stacked canvases, `will-change`, etc.); only the orbit-drag DnD trigger has been
> understood and neutralised.

## Symptôme

Des sprites Three.js (typiquement les labels de sources et de HP, mais aussi les graduations du gizmo) affichent un contenu qui n'est pas le leur :

- soit une copie du framebuffer entier (les labels « avalent » le viewport 3D)
- soit le contenu d'un autre sprite (par exemple tous les labels affichent le même texte, en pratique celui d'un sprite spécifique du gizmo arc)

Le bug est intermittent dans les anciennes versions, mais devient déterministe quand un déclencheur compositeur clair est présent (resize de panneau, transition de visibilité, etc.).

## Mécanisme

Sur WebKitGTK, **le pool de textures GPU est partagé entre le compositeur de la page et le contexte WebGL**. C'est une zone d'intégration historiquement fragile : Chromium cloisonne mieux ces pools, ce qui explique que le bug est essentiellement observé sur la cible Linux.

Toute opération qui force le compositeur à allouer une nouvelle surface GPU au-dessus du canvas WebGL peut produire un alias : un nom de texture GL côté WebGL pointe vers la même mémoire GPU qu'une surface compositeur. Les `CanvasTexture` Three.js des sprites se retrouvent alors à échantillonner le backdrop ou une autre texture du pool.

Triggers connus pour la promotion d'un élément en couche compositeur :

- `backdrop-filter` (blur, etc.)
- `filter` (drop-shadow, blur, etc.)
- l'ajout d'un `<canvas>` supplémentaire empilé au-dessus du canvas WebGL
- très probablement aussi : `transform: translateZ(0)`, `will-change: transform`, `mix-blend-mode`, `isolation: isolate`, `contain: paint`

## Règle dure

**Aucun élément promu en couche compositeur GPU ne doit chevaucher `#omniphony-renderer-mount`.**

En review, refuser :

- toute propriété CSS de la liste ci-dessus appliquée à un élément qui peut être au-dessus du canvas (overlays, panneaux, modales, sticky, etc.)
- l'ajout d'un canvas supplémentaire au-dessus du canvas WebGL

Pour les ombres : préférer `box-shadow` (ne promeut pas de couche) à `filter: drop-shadow`. Pour un effet de glow autour d'un élément : `box-shadow` avec `border-radius` adapté.

## Tentatives infructueuses (decision record)

### Texture des labels en SVG image plutôt que `CanvasTexture`

Commit `7bc8430` (2026-03-27). Hypothèse : les `CanvasTexture` étaient le maillon faible. Réfuté empiriquement : le bug a continué d'apparaître. Le problème n'est pas lié au type de texture côté WebGL, mais à l'allocation GPU côté compositeur.

### Miroir 2D du canvas WebGL

Commit `3a243fa`, annulé par `d4ca167`. Hypothèse : si on insère un `<canvas>` 2D entre le canvas WebGL et les panneaux et qu'on l'alimente en `drawImage` après chaque `renderer.render`, les panneaux pourraient appliquer un `backdrop-filter` qui sample le miroir au lieu du WebGL. Réfuté empiriquement : la surface compositeur du canvas miroir aliase elle-même au boot — tous les labels affichent le même texte avant même la première interaction utilisateur. Le miroir déplace le déclencheur, ne le supprime pas.

### Isolation du canvas WebGL en couche dédiée

Test rapide non commité (`#omniphony-renderer-mount { isolation: isolate; contain: paint; transform: translateZ(0); }`). Réfuté empiriquement : promouvoir explicitement le canvas WebGL en couche compositeur casse les labels dès le boot. La promotion crée le contexte exact qui produit l'aliasing.

## Options pour récupérer un effet de blur sur les panneaux

> **Superseded — see "Resolution (2026-06)" at the top.** On WebKitGTK 2.52.4
> native `backdrop-filter` is viable once the orbit-drag native-DnD trigger is
> neutralised; the fallbacks below were written when no dynamic solution was
> known and are kept for context.

Aucune solution dynamique connue à ce jour ne tient sous WebKitGTK. Les pistes restantes sont :

- **Fond statique décoratif** (gradient, image, texture) entre les panneaux et le canvas. Aucun risque d'aliasing, mais le visuel ne « respire » plus avec la scène 3D.
- **Iframe sandboxant le canvas** : isolation compositeur garantie au prix d'une archi avec IPC postMessage pour tous les events Tauri/state. Heavy, à n'envisager que si le visuel live-blur est critique.
- **Blur appliqué dans le pipeline WebGL lui-même** : pass de post-processing qui écrit derrière les zones de panneau. Sort du WebGL le moins possible, mais demande un investissement non trivial et reste à valider.

## Commits clés

- `7bc8430` (2026-03-27) — `Fix label corruption and add object display colors`. Bascule labels vers SVG image. Mitigation partielle, n'élimine pas le bug.
- `561239b` (2026-04-14) — `Harden Studio overlay DOM access`. Convertit les refs DOM des panneaux en getters lazy. Vraie cause traitée pour un autre symptôme connexe (panneaux remontés). Réintroduit `CanvasTexture` côté labels — sans incidence sur ce bug-ci, contrairement à ce qu'on a longtemps cru.
- `7bf9d2a` (2026-04-28) — `Drop CSS filters above the WebGL canvas`. Suppression des `backdrop-filter` et `filter:drop-shadow` au-dessus du canvas. Fix racine.
- `3a243fa` (2026-04-28) — `Restore overlay backdrop blur via 2D canvas mirror`. Tentative de réactiver le blur via un miroir 2D.
- `d4ca167` (2026-04-29) — Revert du miroir 2D. Le miroir aliase aussi.

## Voir aussi

- `Omniphony/docs/threejs-texture-corruption-notes.md` — historique d'investigation, pré-confirmation de la classe de bug, et observations connexes (corruption des trails diffus).
