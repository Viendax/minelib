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

## 👤 Compte

Au lancement, le jeu propose de **se connecter**, de **créer un compte** ou de **jouer hors
ligne** (solo seulement). Un compte, c'est juste un **pseudo** et un **mot de passe** : ni
e-mail, ni numéro de téléphone.

- Le mot de passe ne quitte jamais le PC : il sert à chiffrer la clé du compte, que le
  relais garde sans pouvoir la lire. Un mot de passe oublié ne se récupère pas.
- **Double authentification** (facultative) : dans *Compte* (en haut à droite de l'écran
  titre), scanne le QR code avec Authy, Aegis, Google Authenticator, 2FAS ou Bitwarden.
- Les pseudos sont inscrits dans un **registre public signé** par le relais : impossible
  de le modifier en douce. Aucune base de données ni donnée personnelle.

### Relais (aucun réglage, aucun port à ouvrir)

Le **relais** met les joueurs en relation, transmet les données des parties (personne ne
voit l'adresse IP des autres), garde les comptes, le classement et les cartes partagées.
Avec `relay=auto` (par défaut), le jeu le trouve tout seul :

1. un relais sur ce PC ou **sur le réseau local** (il s'annonce tout seul) ;
2. sinon, la liste des relais en ligne dans le **DHT de BitTorrent** (« Mainline », des
   millions de nœuds, bibliothèque [mainline](https://github.com/pubky/mainline), MIT) :
   le jeu prend le plus ancien et s'y relie par [iroh](https://iroh.computer)
   (MIT / Apache-2.0), qui perce les box (QUIC) et passe par les relais publics gratuits
   de l'équipe d'iroh quand c'est impossible ;
3. sinon, ce PC devient le relais : joignable de partout sans ouvrir de port, inscrit
   dans le DHT pour les suivants.

Le protocole Minelib reste chiffré de bout en bout (Noise) dans le tunnel.
`minelib-relay.exe` est le même relais sans le jeu : allumé en permanence, il devient
naturellement le relais de tout le monde (le plus ancien est choisi). Une adresse fixe
dans `relay=` reste possible. Limite : les comptes et le classement sont gardés par le
relais choisi ; si un autre relais prend sa place, il faut s'y inscrire.

## 🌐 Multijoueur

Menu **Multijoueur** (compte connecté) : deux grands boutons, **FPS** (logo d'AK) et
**Construction** (cube).

### FPS compétitif (5 contre 5, règles de CS2)

Pas de liste de serveurs : en haut le **classement**, en bas le **choix de la carte**.

- **Classement** : les 50 meilleurs du monde, ou l'onglet **Amis** (ajouter un ami par son
  pseudo). Rang **Elo** calculé comme aux échecs (1200 au départ ; K = 40 les 30
  premières parties, 20 ensuite, 10 au-dessus de 2400 ; Elo moyen de chaque équipe),
  avec les statistiques de CS : matchs, victoires, K/D, % de tirs à la tête, dégâts par
  manche (ADR), étoiles d'homme du match (MVP).
- **Cartes** : chaque carte n'apparaît **qu'une fois**, par sa **signature** (empreinte du
  terrain, des blocs posés, des départs et des sites). Deux copies identiques sont la même
  carte, même sous deux noms ; changer un départ ou un site, c'est une nouvelle carte. Les
  cartes des autres se téléchargent toutes seules (vérifiées par leur signature).
- **Rechercher une partie** : le jeu rejoint une partie qui attend des joueurs sur cette
  carte, ou en ouvre une s'il n'y en a pas (tu joues seul en attendant les autres).
- **Matchmaking** : on ne tombe jamais avec quelqu'un qu'on a signalé (ni qui nous a
  signalés) ; plus un joueur est signalé par partie jouée, plus il est mis avec des
  joueurs dans le même cas.
- À l'arrivée, chacun va chez les **CT** ou les **T** au hasard (5 au plus par équipe).
  Échauffement en attendant (réapparitions, achats gratuits). À **10 joueurs**, compte à
  rebours de 10 s puis la partie commence.
- **Règles de CS2** : 24 manches au plus, premier à 13, changement de côté après 12 ;
  à 12-12, prolongations par blocs de 6 manches (premier à 4, 12 500 $). Manche :
  20 s de départ immobile (achats), 1 min 55 de jeu, 20 s d'achats ; la bombe va à un T
  au hasard, se pose en 3,2 s sur un site, explose au bout de 40 s ; désamorçage en 10 s,
  **5 s avec le kit**. Argent : 800 $ au départ, 16 000 $ au plus, victoire 3 250 $
  (3 500 $ par la bombe), défaite 1 400 $ + 500 $ par défaite d'affilée (3 400 $ au plus),
  +800 $ aux T qui ont posé, primes d'élimination selon l'arme (couteau 1 500 $, fusil à
  pompe 900 $, mitraillette 600 $, AWP 100 $…), 300 $ pour poser ou désamorcer.
- **Achats** (B, dans sa zone de départ) : armes de son camp seulement, grenades,
  **gilet**, **gilet + casque** et **kit de désamorçage** (CT). Le gilet absorbe une part
  des dégâts selon l'arme ; le casque protège la tête.
- Poser la bombe : maintenir **E** (ou **5**) sur un site, immobile. Désamorcer :
  maintenir **E** près de la bombe. Tir sur un coéquipier : un tiers des dégâts.
- Les apparitions sont tirées au hasard dans la zone de son équipe, à **1,5 m** au moins
  les uns des autres.
- Grenades : tout le monde voit les fumées, flashs et feux des autres (même fumée chez
  tous) ; l'hôte calcule les dégâts.
- **Tab** : tableau des scores façon CS (argent des coéquipiers, éliminations, passes,
  morts, MVP, % tête, ADR, score, manches gagnées). En fin de partie, le tableau reste
  affiché avec **victoire / défaite**, et le résultat part au classement : il compte quand
  plus de la moitié des joueurs envoient le même.
- Réseau : simulation à 128 pas par seconde ; chacun prédit son mouvement et se corrige
  sur l'hôte ; les autres sont interpolés (~50 ms) et l'hôte rembobine les cibles pour
  juger les tirs (tête ×4, jambes ×0,75).

### Préparer une carte FPS

Dans son monde (solo, créatif) : sélectionner une zone à la **baguette** (clic gauche,
clic droit), puis :

| Commande | Effet |
|----------|-------|
| `/fps set ct spawn` / `/fps set t spawn` | Zone de départ des CT / des T |
| `/fps set a site` / `b site` / `c site` | Site de bombe A, B ou C |
| `/fps clear …` | Effacer un réglage |
| `/fps info` | Voir les réglages |

Les réglages vont dans `fps.txt`, dans le dossier du monde, et font partie de la
signature de la carte.

### Garant de secours

L'hôte d'une partie FPS envoie chaque seconde l'état complet de la partie (manches, argent,
positions, armes au sol) à un **garant de secours** choisi parmi les joueurs. Si l'hôte
part, le garant reprend la partie là où elle en était, sans la fermer : les autres
joueurs continuent avec lui (l'ancien hôte reste au résultat). Le résultat d'une partie ne
compte au classement que si la majorité des joueurs envoient le même : les joueurs jugent
en cas de désaccord.

### Détails de CS

- Mort : on suit un coéquipier en vie (clic : le suivant).
- Le mort lâche sa meilleure arme ; **G** lâche l'arme principale ; **E** près d'une arme
  au sol la ramasse (échange avec celle du même emplacement). Le sol est vidé à chaque
  manche.
- La bombe se voit sur le dos de celui qui la porte.

### Chat vocal (chiffré de bout en bout)

Maintenir **K** (touche « Micro ») pour parler. En **FPS**, on parle à son équipe ; en
**construction**, aux joueurs proches (48 blocs, plus bas avec la distance). Chaque joueur
chiffre sa voix avec sa propre clé (ChaCha20-Poly1305) et ne la donne qu'à ceux qui doivent
l'entendre, scellée pour chacun (X25519) ; elle change quand quelqu'un ne doit plus
entendre. **Ni le relais, ni l'hôte, ni les garants ne peuvent écouter.** Son : 16 kHz
mono, ADPCM (codec écrit pour Minelib). **Tab** maintenu puis **clic droit** : la souris est
libre et un curseur règle le volume de chaque joueur (0 à 200 %, pour soi seulement,
retenu dans `voice.txt`).

### Bans

Les administrateurs d'un relais (pseudos dans `admins.txt`, dans le dossier du relais)
tapent `/ban <pseudo> [raison]` : le compte est banni **à vie** (inscrit au registre
signé) et ses machines pour **3 mois**. L'empreinte d'une machine n'est jamais envoyée
telle quelle : le jeu n'envoie qu'une empreinte salée par la clé du relais, différente pour
chaque relais.

### Construction en ligne (joueurs illimités)

**Ouvrir un monde** : le monde est envoyé au relais, les autres le téléchargent en
rejoignant. Le monde est découpé en zones de 4 × 4 chunks ; chaque zone active a un
**garant** (un des joueurs) qui valide les blocs posés (bloc valide, dans la zone, 20 par
seconde au plus) et les publie aux joueurs proches. Les garants sont choisis parmi les
joueurs disponibles, **les plus adaptés d'abord** (processeur, images par seconde,
mémoire, temps de présence, réputation) et **les moins chargés** : les zones les plus
actives vont à des garants différents, et le garant d'une zone très active est remplacé
au bout de 10 minutes. Seuls les joueurs proches reçoivent ce qui se passe : le nombre de
joueurs n'est pas limité.

### Commandes en ligne

| Commande | Effet |
|----------|-------|
| `/report <pseudo>` | Signaler un joueur de sa partie (**Tab** complète le pseudo) |
| `/unreport <pseudo>` | Annuler son signalement (on peut de nouveau tomber avec lui) |
| `/ami <pseudo>`, `/ami retirer <pseudo>`, `/amis` | Amis (onglet du classement) |
| `/equipe ct` / `/equipe t` | Changer d'équipe pendant l'échauffement |
| `/ban <pseudo> [raison]` | Bannir (administrateurs du relais) |

## 🔄 Mises à jour automatiques

Au lancement, le jeu regarde la dernière version publiée sur GitHub (Releases de ce
dépôt) : s'il y en a une plus récente, il la télécharge, prend sa place et redémarre
dessus. Pendant le jeu, il revérifie toutes les 10 minutes ; une nouvelle version fait
apparaître le bouton **Mettre à jour** sur l'accueil. Aucune action des joueurs.

Publication : à chaque envoi sur GitHub, `.github/workflows/release.yml` compile le jeu
pour Windows et publie la Release `vNN` si le numéro de version (`VERSION` dans
`crates/app/src/update.rs`) est nouveau. Réglages du dépôt nécessaires une fois :
*Settings → Actions → General* : « Allow all actions » et « Read and write permissions ».
`MINELIB_NO_UPDATE=1` désactive les mises à jour.

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
