<div align="center">

# ⛏️ Minelib

### Construis ton monde. Fais-en ton champ de bataille.

**Un jeu de blocs et un FPS tactique 5 contre 5, dans un seul jeu, avec du raytracing en temps réel.**
Gratuit, sans publicité, sans compte e-mail, sans rien à installer.

![Windows](https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6)
![Linux](https://img.shields.io/badge/Linux-Vulkan-lightgrey)
![Licence](https://img.shields.io/badge/licence-AGPL--3.0-green)
![Rust](https://img.shields.io/badge/fait%20en-Rust-orange?logo=rust)

**[⬇️ Télécharger le lanceur](../../releases/latest/download/minelib-launcher.exe)** · `minelib-launcher.exe` : il télécharge le jeu à jour depuis GitHub et le lance

</div>

---

## Pourquoi Minelib ?

| | |
|---|---|
| 🧱 **Crée sans limite** | Un monde infini à façonner : continents, montagnes, grottes, forêts. Mode créatif, survie, et un **WorldEdit intégré** pour bâtir des villes entières en quelques commandes. |
| 🔫 **Joue en compétitif** | Ta carte devient une arène **5 contre 5 aux règles de CS2** : 34 armes, 5 grenades, économie, bombe, classement Elo. Une carte se prépare en 4 commandes. |
| 💡 **Une image qui claque** | Raytracing en temps réel : miroirs, reflets sur l'eau, la glace et les métaux, lumière colorée des torches, fumigènes en volume qui contournent les murs. |
| 🛡️ **Du fair-play, vraiment** | Les parties sont servies par des joueurs neutres, **rejouées et vérifiées au bit près**, avec un arbitre indépendant en cas de doute. Un facteur de confiance range les tricheurs ensemble. |
| 🔒 **Ta vie privée respectée** | Un pseudo et un mot de passe, c'est tout. Pas d'e-mail, pas de téléphone, personne ne voit ton adresse IP, chat vocal chiffré de bout en bout. |
| ⚡ **Zéro réglage** | Pas de serveur à louer, pas de port à ouvrir : lance le jeu, appuie sur **Jouer**. |

---

## 🎯 Le mode FPS

**La sensation d'un FPS tactique de référence, dans un monde que tu as construit.**

- **Déplacements au poil** : *air strafe*, **bunny hop**, saut accroupi pour monter à 1,5 bloc, et une vitesse qui dépend de l'arme en main (couteau rapide, AWP lourde).
- **Des dégâts qui ne pardonnent pas** : valeurs de CS2 pour chaque arme, perte avec la distance, tête ×4, gilet et casque. Les balles traversent un mur fin.
- **34 armes et 5 grenades** : pistolets, mitraillettes, fusils à pompe, fusils d'assaut, fusils de précision ; HE, flash, fumigène, molotov et leurre (qui imite *ton* arme principale).
- **Tout ce qu'on attend d'un compétitif** : menu d'achat par camp, économie, bombe et kit, prolongations, tableau des scores, fil des éliminations avec tirs en tête, à travers un mur ou une fumée, armes et grenades qu'on lâche et qu'on ramasse.
- **Ton équipe, d'un coup d'œil** : un triangle à la couleur de chaque coéquipier, visible à travers les murs, et son nom quand tu le vois ; en haut, le bandeau des joueurs des deux camps (les morts s'assombrissent), et ta couleur préférée parmi vert, jaune, orange, bleu et violet.
- **Minimap** en haut à droite, avec les règles de CS : tes coéquipiers toujours, les adversaires seulement quand ils sont repérés, les sites de bombe et la bombe quand ton équipe la connaît.
- **Réseau à 256 Hz** : positions envoyées 256 fois par seconde, prédiction et rembobinage des tirs. À 10 joueurs, l'hôte envoie moins de 1 Mo par seconde.

### Jouer en ligne

Ouvre **Multijoueur → FPS**, choisis une carte et appuie sur **Jouer**.

- **Joue avec tes amis** : le panneau de droite liste tes amis (en ligne en vert) ; un clic invite dans ton **lobby** (5 joueurs). Celui qui invite en premier est l'hôte du lobby : il lance la partie pour tout le monde, et vous êtes **toujours dans la même équipe**. Clic droit sur un membre : *Exclure* ou *Promouvoir hôte*.

- Chaque carte affiche clairement son format (**5 contre 5**, 2 contre 2…). **Seules les cartes 5 contre 5 comptent au classement.**
- **Les cartes de la communauté** : toute carte déjà lancée en partie FPS en ligne apparaît dans la liste de tout le monde et se télécharge toute seule. Elle reste disponible tant qu'un joueur qui y a joué l'a encore (son jeu la renvoie au relais si besoin). Tes autres mondes restent privés.
- Pendant l'échauffement, **les autres joueurs restent anonymes** (« Joueur 3 ») : on ne quitte pas la recherche parce qu'on a vu un nom. Les pseudos s'affichent quand la partie commence au complet.
- **Matchmaking équitable** : d'abord un niveau Elo proche de l'équipe déjà formée (moyenne du lobby), puis les nouveaux avec les nouveaux, puis un facteur de confiance proche (le plus bas du lobby). Jamais avec quelqu'un que tu as signalé.
- **Classement** : tes amis d'abord, le **Top mondial** à côté. Elo calculé comme aux échecs sur les parties 5 contre 5 des 10 cartes les plus jouées, avec les statistiques de CS (K/D, % de tirs à la tête, ADR, MVP).

### Le facteur de confiance

Chaque compte a un **facteur de confiance** de **0 à 1000**. Un nouveau compte commence à **500**. Il sépare trois paliers, et les parties se forment entre joueurs du même palier :

- **joueurs corrects** : vers **900 à 1000** ;
- **trolls** (souvent signalés, partent en pleine partie) : vers **250 à 350** ;
- **tricheurs** : **sous 200**, avant même d'être bannis.

| Événement | Effet |
|---|---|
| Partie finie sans signalement compté, sans absence ni soupçon | **+12**, plus 1 par tranche de 150 points déjà acquis (+15 à 500, +18 à 1000) |
| Signalé par **un seul** joueur de la partie | **0** (un mauvais perdant seul ne coûte rien) |
| Signalé par **2 joueurs ou plus** de la même partie | **−25 par joueur** qui a signalé |
| Absence de moins de 2 minutes | **0** |
| Absence plus longue | **−5 à −90** selon la part de la partie manquée (sur 30 min : 5 min ≈ −8, 10 min ≈ −33, 15 min = −75) |
| Abandon (absent 90 % de la partie ou plus) | **−100** |
| Soupçon du vérificateur dans la partie | **−80** |
| Triche prouvée | **−400**, et bannissement si l'arbitre la confirme |

Les signalements et les absences ne font **jamais descendre sous 300** : c'est le palier des trolls. Seuls les soupçons du vérificateur et la triche prouvée vont plus bas. Un signalement retiré pendant la partie ne compte pas.

Ces valeurs ont été choisies par simulation (2 000 joueurs, 90 jours, en testant aussi deux fois plus de faux signalements que prévu) : les joueurs corrects restent au-dessus de 860 même dans le pire cas, et les trois paliers ne se mélangent pas.

Le facteur est **lié à la machine** : c'est le plus bas des comptes utilisés sur ton PC qui compte. Recréer un compte ne remet pas les compteurs à zéro.

**Déconnecté ?** Appuie sur **Jouer** : tu reviens dans ta partie en cours.
**Tir sur les alliés** : au 3ᵉ coéquipier tué, tu es exclu de la partie (sans perte de confiance pour ça seul, sauf signalements).

### Anti-triche

- **Des serveurs neutres** : la partie est servie par un **garant indépendant**, sans aucun lien avec les joueurs (ni ami, ni signalé). De préférence, c'est un joueur qui **vient de lancer sa propre partie** : il a tout intérêt à rester en jeu. Son jeu sert l'autre partie en arrière-plan, sans toucher à ses images, et un autre garant le **relève avant la fin de sa partie**, sans coupure. À défaut, un joueur au menu sert.
- **Tout est rejoué** : un second garant reçoit le journal complet de la partie, chaîné et infalsifiable, et **rejoue chaque déplacement, chaque tir, chaque grenade** avec le même code. Le moindre écart est une preuve.
- **Un arbitre tranche** : en cas d'accusation, le relais confie ses propres pièces à un troisième garant sans lien avec personne. Triche confirmée par l'arbitre : **bannissement automatique** du compte et de la machine.
- **Contre les wallhacks** : un adversaire caché derrière un mur n'est tout simplement pas envoyé à ton jeu.
- **Le résultat d'une partie** ne compte que si la majorité des joueurs **et** le vérificateur trouvent le même.

### Crée ta carte

Dans ton monde en créatif, sélectionne une zone avec la baguette (`//wand`), puis :

| Commande | Effet |
|----------|-------|
| `/fps set spawn ct` · `/fps set spawn t` | Zone de départ des CT / des T |
| `/fps set site a` · `b` · `c` | Site de bombe |
| `/fps set maxplayer <n>` | Nombre de joueurs (2 à 10 ; 10 = classé) |
| `/fps info` · `/fps clear …` | Voir ou effacer les réglages |

Les mots peuvent être dans n'importe quel ordre (`/fps set ct spawn` marche aussi). Ta carte est partagée avec tout le monde dès que tu lances une partie dessus.

---

## 🧱 Le mode construction

- **Un monde infini** généré à la volée, avec un cycle jour/nuit, des grottes et des forêts.
- **Survie, créatif, spectateur** (`/gm 0`, `1`, `3`).
- **WorldEdit intégré** : plus de 40 commandes `//` (remplir, copier, coller, tourner, sphères, cylindres, annuler…).
- **Des blocs qui se posent comme on s'y attend** : escaliers face à toi (à l'envers au plafond), dalles, portes, trappes, vitres qui se raccordent, bannières.
- **Construis à plusieurs, sans limite de joueurs** : ouvre ton monde en ligne, il continue même si tu pars. Le créateur est OP et gère son serveur (`/op`, `/ban`, `/kill`).
- **Importe tes mondes Java** (format Anvil, toutes versions) et tes **packs de textures** : les fichiers d'origine ne sont jamais modifiés.

---

## 👤 Ton compte

- **Pseudo + mot de passe**, rien d'autre. Ton mot de passe ne quitte jamais ton PC.
- **Double authentification** facultative (Authy, Aegis, Google Authenticator, 2FAS, Bitwarden), demandée **une seule fois par appareil**.
- **Connexion automatique** au lancement, sans retaper ton mot de passe ni ton code.
- **Un pseudo est unique** sur tous les relais, inscrit dans un registre public signé.
- **Amis** : ajoute-les depuis le jeu, retrouve-les dans le classement.
- **Ton skin** : dans **Paramètres**, un aperçu en 3D de ton personnage et le bouton **Changer de skin** (PNG de 64 × 64 au format des jeux de blocs). Les autres joueurs le voient ; il change en direct en construction, et reste le même pendant une partie FPS. Pendant l'échauffement, tout le monde garde le modèle par défaut (anonymat).
- **7 langues** : français, English, español, Deutsch, italiano, português, Nederlands (Paramètres → Langue).
- **Chat vocal chiffré de bout en bout** : à ton équipe en FPS, aux joueurs proches en construction. Ni le relais, ni l'hôte ne peuvent écouter.

---

## ⌨️ Commandes

Les touches sont **physiques** (même place en AZERTY et en QWERTY) et se règlent dans **Échap → Options → Régler les touches**, avec un onglet Build et un onglet FPS. La **sensibilité de la souris** se règle séparément pour chaque mode.

<details>
<summary><b>Construction</b></summary>

| Touche | Action | | Touche | Action |
|--------|--------|-|--------|--------|
| Z Q S D | Se déplacer | | Clic gauche | Casser |
| Espace | Sauter / monter | | Clic droit | Poser / ouvrir |
| Maj gauche | S'accroupir / descendre | | Clic molette | Copier le bloc visé |
| Ctrl gauche | Courir | | 1 à 0, molette | Barre d'objets |
| E | Inventaire | | T / `/` | Tchat / commande |
| C (maintenu) | Zoom | | F5 | Vue 1re / 3e personne |
| F2 | Capture d'écran | | F3 | Écran de debug |

</details>

<details>
<summary><b>FPS</b></summary>

| Touche | Action | | Touche | Action |
|--------|--------|-|--------|--------|
| Z Q S D | Se déplacer | | Clic gauche | Tirer / lancer fort |
| Espace | Sauter (bhop) | | Clic droit | Lunette / lancer en cloche |
| Ctrl gauche | S'accroupir | | R | Recharger |
| Maj gauche | Marcher (silencieux) | | B | Menu d'achat |
| 1 / 2 / 3 / 4 | Principale / pistolet / couteau / grenades | | F4 | Racheter |
| G | Lâcher l'arme ou la grenade | | E | Ramasser, poser, désamorcer |
| Tab | Scores | | K (maintenu) | Micro |

</details>

<details>
<summary><b>Commandes du tchat</b></summary>

Le tchat se navigue avec ← → (Début, Fin, Suppr).

| Commande | Effet |
|----------|-------|
| `/help` | Toutes les commandes |
| `/gm <0-3>` | Mode de jeu |
| `/kill [pseudo]` | Mourir, ou tuer un joueur (OP d'un serveur de construction) |
| `/report <pseudo>` · `/unreport <pseudo>` | Signaler un joueur de ta partie, ou annuler |
| `/ami <pseudo>` · `/ami retirer <pseudo>` · `/amis` | Amis (aussi `/friend add <pseudo>`) |
| `/equipe ct` · `/equipe t` | Changer d'équipe pendant l'échauffement (aussi `/team`) |
| `/op` · `/deop` · `/ban` · `/unban` | Gérer son serveur de construction |
| `//…` | WorldEdit (créatif) |

</details>

---

## 🚀 Installation

**Windows** : télécharge **`minelib-launcher.exe`** (lien en haut de la page) et lance-le. Le lanceur ne contient pas le jeu : il demande à GitHub la dernière version publiée, la télécharge avec une barre de progression, vérifie son empreinte SHA-256, puis lance le jeu. Les fois suivantes, il ne télécharge que s'il y a une nouvelle version (et si GitHub est injoignable, il lance celle déjà installée). Le jeu est rangé dans `%APPDATA%\.minelib\bin`.

Le jeu compare aussi son propre fichier à la version officielle (« fichiers vérifiés », ou « exécutable non officiel » s'il a été modifié ou abîmé : une alerte honnête, pas une protection absolue, ce sont les garants qui empêchent la triche). La ligne en bas à gauche de l'accueil indique la version, par exemple « Minelib v77 - à jour (lanceur) ».

Tes mondes, captures et réglages sont dans `%APPDATA%\.minelib` (Windows) ou `~/.minelib` (Linux). Dépose un monde Java dans `saves/` ou un pack de textures dans `resourcepacks/`.

| Plateforme | État |
|------------|------|
| Windows 10/11 (DirectX 12, Vulkan) | ✅ |
| Linux (Vulkan) | ✅ |
| macOS (Metal) | bientôt |
| Android, iOS | en préparation |

---

<details>
<summary><h2>🛠️ Pour les développeurs</h2></summary>

### Compiler

```bash
git clone <url-du-dépôt> minelib && cd minelib
cargo run --release                                     # le jeu
cargo build --release --target x86_64-pc-windows-gnu    # l'exe Windows depuis Linux
cargo test --release && cargo clippy --release --all-targets
```

Il faut Rust 1.85 ou plus récent.

### Organisation

```text
crates/
├── platform/   fenêtre, entrées, temps (winit)
├── render/     rendu wgpu : chunks, ciel, interface, raytracing, armes
├── world/      blocs, génération, lumière, maillage
├── assets/     dossier .minelib, options, packs de textures
├── net/        protocole, chiffrement (Noise), client du relais
├── relay/      relais : comptes, classement, cartes, parties, arbitrage
├── p2p/        découverte des relais (DHT) et tunnels (iroh)
└── app/        le jeu : menus, modes, FPS, WorldEdit, réseau de partie
```

### Relais

Le relais met les joueurs en relation, transmet les données des parties, garde les comptes, le classement et les cartes. Le jeu le trouve tout seul (réseau local, puis DHT de BitTorrent, sinon ce PC devient le relais, joignable sans ouvrir de port grâce à iroh). `minelib-relay.exe` est le même relais sans le jeu ; les administrateurs (`admins.txt`) peuvent bannir un tricheur avec `/bantriche <pseudo> <preuve>`.

### Publication

À chaque envoi sur GitHub, `.github/workflows/release.yml` compile le jeu pour Windows et publie la Release `vNN` si `VERSION` (`crates/app/src/update.rs`) est nouveau. `MINELIB_NO_UPDATE=1` désactive les mises à jour.

### Variables d'environnement

`MINELIB_HOME`, `MINELIB_WORLD`, `MINELIB_SEED`, `MINELIB_VIEW`, `MINELIB_VSYNC`, `MINELIB_FPS_MAX`, `MINELIB_DEBUG`, `MINELIB_NIGHT`, `MINELIB_START=x,y,z[,lacet,tangage]`.

</details>

---

## 📜 Licences et crédits

- **Code** : [GNU AGPL v3](LICENSE).
- **Textures (VoxelArt Libre)** : *Pixel Perfection* de XSSheep et la communauté (CC BY-SA 4.0), Mineclonia (CC BY-SA 4.0 / CC0), *Minecraft-Textures* d'Auseawesome (CC0), verre d'après le *Voxel Pack* de Kenney (CC0). Détail : `assets/CREDITS_VoxelArt_Libre.txt`.
- **Modèles d'armes** : *Ultimate Gun Pack* de [Quaternius](https://quaternius.com) (CC0).
- **Sons** : bibliothèques libres (CC0). Détail : `crates/app/assets/sounds/CREDITS.txt`.
- **Bibliothèques** : wgpu, winit, rodio, mainline, iroh et d'autres, sous licences MIT / Apache-2.0.

Minelib est un projet indépendant, sans lien avec Mojang, Microsoft ou Valve. Minecraft et Counter-Strike sont des marques de leurs propriétaires respectifs.
