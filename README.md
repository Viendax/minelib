<div align="center">

# ⛏️ Minelib

**Moteur de jeu voxel / FPS multiplateforme, écrit en Rust de A à Z.**

Construire comme dans un jeu de blocs, se battre comme dans un FPS tactique,
avec du raytracing en temps réel, dans un seul moteur original.

![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange?logo=rust)
![wgpu](https://img.shields.io/badge/rendu-wgpu%2025-blue)
![Plateformes](https://img.shields.io/badge/Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)
![Licence](https://img.shields.io/badge/licence-AGPL--3.0-green)

</div>

---

## ✨ En bref

- 🧱 **Monde voxel infini** : génération procédurale (continents, collines, montagnes,
  grottes en « spaghetti », arbres), chunks de 16 × 16 × 256 chargés en fond sur tous les cœurs.
- 💡 **Lumière** : soleil, cycle jour/nuit, torches à lumière colorée (13 sels chimiques,
  vrai « test de flamme »), occultation ambiante, lumière qui respecte la forme des blocs
  (dalles, escaliers…).
- 🪞 **Raytracing en temps réel** : miroirs (jusqu'à 4 rebonds), reflets sur l'eau, la glace
  et les métaux (fer, or, diamant : reflet proche et flou).
- 🔫 **Mode FPS** : déplacements façon Counter-Strike (bunny hop, fatigue), 34 armes,
  5 grenades, menu d'achat, lunettes, fumigènes qu'on perce à la balle.
- 🛠️ **WorldEdit intégré** : baguette de sélection et plus de 40 commandes `//…`.
- 📦 **Compatible Minecraft** : import des mondes Java (Anvil) et des packs de textures.
- 🖥️ **Multiplateforme** : un seul code pour Windows, Linux et macOS.

---

## 🚀 Démarrage rapide

### Windows

Téléchargez `minelib_vXX.exe` (onglet *Releases* ou racine du dépôt) et lancez-le.
Aucune installation n'est nécessaire.

### Depuis les sources

```bash
git clone <url-du-dépôt> minelib
cd minelib
cargo run --release
```

Il faut **Rust 1.85 ou plus récent** ([rustup.rs](https://rustup.rs)) et un pilote graphique
récent (Vulkan, DirectX 12 ou Metal).

| Plateforme | API graphique (via wgpu) | État |
|------------|--------------------------|------|
| Windows 10/11 | DirectX 12 ou Vulkan | ✅ testé |
| Linux | Vulkan | ✅ testé |
| macOS | Metal | ⚠️ prévu par wgpu, pas encore testé |

Compiler l'exécutable Windows depuis Linux :

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

---

## 📁 Dossier du jeu, mondes et packs de textures

Toutes les données du joueur sont dans le dossier **`.minelib`** :

| Système | Emplacement |
|---------|-------------|
| Windows | `%APPDATA%\.minelib` (touches **Win + R**, tapez `%appdata%`, puis ouvrez `.minelib`) |
| Linux / macOS | `~/.minelib` |

```text
.minelib/
├── saves/           un dossier par monde (Minelib ou Minecraft importé)
├── resourcepacks/   packs de textures (.zip ou dossiers)
├── screenshots/     captures d'écran (F2)
├── logs/            journal du jeu
├── options.txt      réglages
└── keys.txt         touches personnalisées
```

### Importer un monde Minecraft

Copiez le dossier du monde (celui qui contient `level.dat` et `region/`) dans
`.minelib/saves/`. Il apparaît dans la liste **Solo**.

- Java Edition, format Anvil : avant 1.13, de 1.13 à 1.17, et 1.18 ou plus récent.
- Les fichiers d'origine **ne sont jamais modifiés**. Minelib écrit sa propre sauvegarde
  à côté (`level.txt`, `chunks.bin`, `edits.bin`).
- La hauteur est de 256 blocs : un monde 1.18+ (Y −64 à 319) est décalé de +64, et ce qui
  dépasse Y 191 est ignoré.

### Ajouter un pack de textures

Déposez le `.zip` (ou le dossier) dans `.minelib/resourcepacks/`, puis choisissez-le dans
**Échap → Packs de textures**. Les noms de textures de Minecraft, anciens comme récents,
sont reconnus. Le pack par défaut, *VoxelArt Libre*, est sous licences libres.

---

## 🧱 Construction

- **Dalles** : posées sur le dessus d'un bloc → dalle du bas ; au plafond → dalle du haut ;
  contre un mur → dalle verticale (accroupi : du haut ou du bas selon l'endroit visé).
  Deux dalles du même matériau dans une case forment un bloc complet.
- **Portes** : 2 blocs de haut, s'ouvrent et se ferment au clic droit (les deux moitiés).
  Accroupi, le clic droit pose le bloc tenu au lieu d'ouvrir (portes et trappes).
- **Trappes** : en haut ou en bas de la case ; clic droit pour les relever ou les rabattre.
- **Verre** (incolore et des 16 couleurs, même motif) : deux blocs collés se raccordent, le
  cadre n'entoure que l'ensemble.
- **Bannières** : 2 blocs de haut (posées comme en objet), bord seulement autour du
  rectangle ; la moitié du bas ne s'affiche pas dans un bloc plein (seulement dans le vide
  ou derrière du verre).
- **Vue F5** : 1re personne, 3e personne de dos ou de face (4 blocs, comme Minecraft),
  avec le modèle du joueur animé. Désactivée en mode FPS.

## 🎮 Modes de jeu

Changez de mode avec `/gm <n>` (ou `/gamemode <n>`) :

| `/gm` | Mode | Description |
|:----:|------|-------------|
| `0` | **Survie** | Santé, dégâts de chute, inventaire (piles de 64). `/fly` autorise le vol. |
| `1` | **Créatif** | Blocs illimités (rien n'est consommé), vol (double appui sur Espace), WorldEdit. L'inventaire (E) montre le catalogue, l'inventaire de survie et la barre rapide. |
| `2` | **FPS** | Combat façon Counter-Strike : armes, grenades, argent, menu d'achat. |
| `3` | **Spectateur** | Vol forcé, on traverse les blocs, aucune interaction. |

---

## ⌨️ Commandes

Les touches sont **physiques** : elles restent à la même place en AZERTY comme en QWERTY
(la touche « W » est le **Z** d'un clavier AZERTY). Tout se règle dans
**Échap → Options → Régler les touches**, avec un onglet **Build** et un onglet **FPS**.
On peut choisir « maintenir » ou « bascule » pour marcher et s'accroupir.

### Construction (survie, créatif)

| Touche | Action | | Touche | Action |
|--------|--------|-|--------|--------|
| Z Q S D | Se déplacer | | Clic gauche | Casser |
| Espace | Sauter / monter | | Clic droit | Poser |
| Maj gauche | S'accroupir / descendre | | Clic molette | Copier le bloc visé |
| Ctrl gauche | Courir | | 1 à 0, molette | Barre d'objets |
| E | Inventaire | | T / `/` | Tchat / commande |
| C (maintenu) | Zoom | | N | Jour / nuit |
| F2 | Capture d'écran | | F3 | Écran de debug |
| Page ↑ / ↓ | Distance de rendu | | Échap | Menu |
| F5 | Vue 1re / 3e personne (dos, face) | | Clic droit sur porte / trappe | Ouvrir / fermer |

### FPS

| Touche | Action | | Touche | Action |
|--------|--------|-|--------|--------|
| Z Q S D | Se déplacer | | Clic gauche | Tirer / lancer fort |
| Espace | Sauter (bhop) | | Clic droit | Lunette / lancer en cloche |
| Ctrl gauche | S'accroupir | | R | Recharger |
| Maj gauche | Marcher (silencieux) | | B | Menu d'achat |
| 1 / 2 / 3 / 4 | Principale / pistolet / couteau / grenades | | F4 | Racheter |
| Molette | Arme suivante / précédente | | A | Dernière arme |
| G | Lâcher l'arme | | Clic molette | Signaler (ping) |

### Commandes du tchat

| Commande | Effet |
|----------|-------|
| `/help` | Liste des commandes |
| `/gm <0-3>` | Changer de mode de jeu |
| `/fly` | Autoriser ou interdire le vol en survie |
| `/heal` | Rendre toute la santé |
| `/give <bloc> [nombre]` | Obtenir un bloc (ex. `/give mirror`, `/give iron_block`) |
| `//…` | Commandes WorldEdit (créatif, voir plus bas) |

Le tchat accepte jusqu'à 256 caractères et coupe les messages longs sur plusieurs lignes.

---

## 🔫 Mode FPS

- **Déplacements de Counter-Strike** : accélération dans les airs (*air strafe*), friction au
  sol, **bunny hop**, et fatigue des jambes si l'on saute ou s'accroupit trop souvent.
- **34 armes** :
  - **Pistolets** : Glock-18, USP-S, P2000, Dual Berettas, P250, Five-SeveN, Tec-9,
    CZ75-Auto, Desert Eagle, R8 Revolver.
  - **Pistolets-mitrailleurs** : MAC-10, MP9, MP7, MP5-SD, UMP-45, P90, PP-Bizon.
  - **Fusils à pompe** : Nova, XM1014, Sawed-Off, MAG-7.
  - **Mitrailleuses** : M249, Negev.
  - **Fusils** : Galil AR, FAMAS, AK-47, M4A4, M4A1-S, SG 553, AUG.
  - **Fusils de précision** : SSG 08, AWP, G3SG1, SCAR-20.
- **Précision réaliste** : la dispersion augmente avec la vitesse, une rafale part droit
  quand on est immobile, et un tir sans lunette à l'arrêt depuis 0,2 s est plus précis.
- **Lunettes** : ×2 pour l'AUG et la SG 553 ; ×2 puis ×4 pour les fusils de précision.
  Le R8 doit armer son chien avant de tirer.
- **Équipement comme dans CS** : une arme principale, un pistolet, un couteau et jusqu'à
  4 grenades (2 flashs au plus). Il faut attendre 0,75 s après un changement d'arme avant
  de tirer.
- **Grenades** :
  - **HE** : explosion à dégâts dégressifs.
  - **Flash** : l'aveuglement dépend de l'angle de vue.
  - **Fumigène** : 18 s, fumée en volume calculée par la carte graphique. Elle remplit un
    volume fixe en contournant les murs (elle s'étale dans les couloirs, comme dans CS2),
    opaque vue de dehors comme de dedans. Chaque balle y perce un tout petit trou qui se
    referme en 0,75 s. Elle remplit aussi le vide des blocs non pleins (trappes, dalles…).
  - **Molotov** : feu au sol.
  - **Leurre**.
- **Sons** : tir, rechargement, lunette, sortie de chaque arme et couteau. Tous les sons
  sont libres (CC0).

---

## 🛠️ WorldEdit

Disponible en **créatif**. `//wand` donne la **baguette de sélection** :

- **clic gauche** : coin 1 ;
- **clic droit** : coin 2 ;
- **clic molette** : effacer la sélection.

La zone s'affiche en bleu tant qu'on tient la baguette.

| Catégorie | Commandes |
|-----------|-----------|
| **Sélection** | `//pos1` `//pos2` (pieds), `//hpos1` `//hpos2` (bloc visé), `//sel` (effacer), `//size`, `//wand` |
| **Zone** | `//expand <n> [direction \| vert]`, `//contract <n> [direction]`, `//shift <n> [direction]` |
| **Remplir** | `//set <motif>`, `//replace [<de>] <vers>`, `//walls`, `//faces` (`//outline`), `//overlay`, `//center`, `//naturalize`, `//line`, `//drain [rayon]`, `//replacenear <rayon> <de> <vers>` |
| **Formes** (aux pieds) | `//sphere` `//hsphere <motif> <rayon>`, `//cyl` `//hcyl <motif> <rayon> [hauteur]`, `//pyramid` `//hpyramid <motif> <taille>` |
| **Déplacer** | `//move <n> [direction]`, `//stack <n> [direction]` |
| **Presse-papiers** | `//copy`, `//cut`, `//paste [-a]` (`-a` : sans l'air), `//rotate [90 \| 180 \| 270]`, `//flip [direction]` |
| **Historique** | `//undo`, `//redo` (30 opérations) |
| **Infos** | `//count <blocs>`, `//distr`, `//help` |

- **Motifs** : un nom français ou une clé (`pierre`, `stone`, `planks_oak`,
  `slab:stone:bottom`), un numéro de bloc, ou un mélange : `//set 50%stone,50%dirt`.
- **Directions** : `me` (le regard, par défaut), `back`, `left`, `right`, `up`, `down`,
  `north`, `south`, `east`, `west` (ou `n`, `s`, `e`, `w`, `u`, `d`).
- **Limites** : 4 millions de blocs par commande, uniquement dans les chunks chargés.
  `//rotate` ne réoriente pas encore les escaliers ni les torches.

---

## 🖥️ Rendu et technique

| Domaine | Détails |
|---------|---------|
| **API graphique** | [wgpu](https://wgpu.rs) 25 : DirectX 12, Vulkan ou Metal |
| **Profondeur** | Z inversé et projection infinie : précision constante à plusieurs km |
| **Visibilité** | Élimination hors champ (frustum), tri de l'avant vers l'arrière, occlusion par tranches de chunk |
| **Lumière** | Propagation par blocs avec couverture 4 × 4 par face, lumière colorée RVB, AO lissée |
| **Raytracing** | Grille de voxels sur le GPU (cubes, dalles, escaliers, clôtures, murets, panneaux, torches) ; miroirs, eau, glace, métaux |
| **Image** | Échelle de rendu réglable, anticrénelage temporel, VSync ou FPS illimités, mode faible latence |
| **Multicœur** | Génération et maillage sur un pool de threads basse priorité, jeu et rendu en haute priorité |
| **GPU hybrides** | Détection de la carte dédiée et réglages adaptés (dédiée, intégrée, logiciel) |
| **Mémoire** | Distance de rendu (2 à 64 chunks) plafonnée selon la mémoire vive disponible |
| **Audio** | [rodio](https://github.com/RustAudio/rodio) (Ogg Vorbis), fil audio dédié |
| **Fenêtre et entrées** | [winit](https://github.com/rust-windowing/winit) 0.30, touches physiques |

### Organisation du code

```text
crates/
├── platform/   fenêtre, boucle d'événements, entrées, temps (winit)
├── render/     rendu wgpu : chunks, ciel, interface, raytracing, armes en main
├── world/      blocs, chunks, génération, lumière, maillage, lancer de rayons
├── assets/     dossier .minelib, options, packs de textures
└── app/        le jeu : menus, joueur, modes, FPS, WorldEdit, sauvegardes, import Minecraft
```

### Variables d'environnement (développement)

| Variable | Effet |
|----------|-------|
| `MINELIB_HOME=<dossier>` | Remplace le dossier `.minelib` |
| `MINELIB_WORLD=<nom>` | Ouvre (ou crée) directement ce monde |
| `MINELIB_SEED=<n>` | Graine du monde créé par `MINELIB_WORLD` |
| `MINELIB_VIEW=<chunks>` | Distance de rendu au lancement |
| `MINELIB_VSYNC=1` | Démarrer avec la VSync |
| `MINELIB_FPS_MAX=<n>` | Plafond de FPS |
| `MINELIB_DEBUG=1` | Écran F3 ouvert au démarrage |
| `MINELIB_NIGHT=1` | Commencer de nuit |
| `MINELIB_START=x,y,z[,lacet,tangage]` | Position de départ |

### Tests

```bash
cargo test --release
cargo clippy --release --all-targets
```

---

## 📜 Licences et crédits

- **Code** : [GNU AGPL v3](LICENSE).
- **Textures (VoxelArt Libre)** : *Pixel Perfection* de XSSheep et la communauté
  (CC BY-SA 4.0), Mineclonia (CC BY-SA 4.0 / CC0), *Minecraft-Textures* d'Auseawesome
  (CC0), verre d'après le *Voxel Pack* de Kenney (CC0). Le détail est dans `assets/CREDITS_VoxelArt_Libre.txt`.
- **Modèles d'armes** : *Ultimate Gun Pack* de [Quaternius](https://quaternius.com) (CC0).
- **Sons** : *Free Firearm Sound Library*, *gun reload sounds*, *Fantasy Weapons and
  Apparel SFX Library*, *Tinysized SFX* et d'autres, tous en CC0. Le détail est dans
  `crates/app/assets/sounds/CREDITS.txt`.

Minelib est un projet indépendant, sans lien avec Mojang, Microsoft ou Valve.
Minecraft et Counter-Strike sont des marques de leurs propriétaires respectifs.
