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
| 🔫 **Joue en compétitif** | Ta carte devient une arène **5 contre 5 en compétitif** : 34 armes, 5 grenades, économie, bombe, classement Elo. Une carte se prépare en 4 commandes. |
| 💡 **Une image qui claque** | Miroirs qui montrent tout, toi compris (même à la première personne) et la lumière de tes projecteurs, et deux miroirs face à face qui se renvoient leurs reflets en couloir, sans raytracing ; raytracing en temps réel pour les reflets sur l'eau, la glace et les métaux ; lumière colorée des torches, fumigènes en volume qui contournent les murs. |
| 🛡️ **Du fair-play, vraiment** | Les parties sont servies par des joueurs neutres, **rejouées et vérifiées au bit près**, avec un arbitre indépendant en cas de doute. Un facteur de confiance range les tricheurs ensemble. |
| 🌍 **Joueurs illimités** | Un seul monde peut accueillir la planète entière : il se découpe tout seul en régions, servies par les jeux des joueurs. Tu ne vois que les 2 048 joueurs les plus proches de toi, et tu entends au vocal ceux qui sont à côté, où que tu sois. |
| 🔒 **Ta vie privée respectée** | Un pseudo et un mot de passe, c'est tout. Pas d'e-mail, pas de téléphone, personne ne voit ton adresse IP, chat vocal chiffré de bout en bout. |
| ⚡ **Zéro réglage** | Pas de serveur à louer, pas de port à ouvrir : lance le jeu, appuie sur **Jouer**. |

---

## 🎯 Le mode FPS

**La sensation d'un FPS tactique de référence, dans un monde que tu as construit.**

- **Déplacements au poil** : *air strafe*, **bunny hop**, saut accroupi pour monter à 1,5 bloc, et une vitesse qui dépend de l'arme en main (couteau rapide, AWP lourde).
- **Des dégâts qui ne pardonnent pas** : des valeurs propres à chaque arme, perte avec la distance, tête ×4, gilet et casque. Les personnages ont des proportions réalistes (petite tête, longues jambes) et leur hitbox les suit : la tête, plus étroite que les épaules, se vise. Accroupi, la tête reste posée sur le cou et avance avec lui : sa hitbox la suit. Les balles traversent un mur fin.
- **34 armes et 5 grenades** : pistolets, mitraillettes, fusils à pompe, fusils d'assaut, fusils de précision ; HE, flash, fumigène, molotov et leurre (qui imite *ton* arme principale).
- **Tout ce qu'on attend d'un compétitif** : menu d'achat par camp, économie, bombe et kit, prolongations, tableau des scores, fil des éliminations avec tirs en tête, à travers un mur ou une fumée, armes et grenades qu'on lâche et qu'on ramasse.
- **Menu d'achat (B)** : il reste ouvert après un achat ; un clic droit sur un achat de la manche le revend au prix payé, tant qu'on peut encore acheter. Personne ne tire tant que la manche n'a pas commencé, ni pendant les pauses.
- **La bombe posée bipe**, de plus en plus vite, jusqu'à l'explosion, qu'on entend de loin. Son porteur l'a sur le dos, debout ; pour lui, elle s'affiche en bas à droite avec ses armes (touche 5).
- **Ce que tiennent les autres joueurs** : le vrai modèle de leur arme (tenue à deux mains, pointée où ils regardent), de leur couteau, de leur grenade ou de la bombe.
- **Ton équipe, d'un coup d'œil** : un triangle à la couleur de chaque coéquipier, visible à travers les murs, et son nom quand tu le vois ; en haut, le bandeau des joueurs des deux camps (la couleur de chaque joueur sous sa tête, dans les deux équipes ; les morts grisés, avec une croix rouge), et ta couleur préférée parmi vert, jaune, orange, bleu et violet.
- **Votes et pauses** (Échap pendant une partie) : proposer d'exclure un coéquipier (tous les autres doivent être d'accord) ou une pause tactique (la majorité ; 3 par équipe, 30 s, au départ de la manche suivante). Le vote s'affiche à gauche : F1 oui, F2 non. Un joueur qui se déconnecte déclenche une pause technique jusqu'à son retour (5 minutes au plus) ; le temps de pause ne compte pas dans les absences.
- **Un vrai ciel** : sa couleur vient de la diffusion de la lumière dans l'air (bleu de midi, horizon doré au lever et au coucher, bleu nuit), avec le soleil et son halo, une grande lune (ses mers, ses cratères, son relief le long de l'ombre, ses phases, rousse quand elle est basse), les étoiles, et des **nuages en volume** éclairés par le soleil ou la lune, qui dérivent avec le vent. Presque gratuit à l'image : l'atmosphère et les nuages sont calculés à part, un peu à chaque image, et le ciel se lit ensuite en deux ou trois lectures de texture.
- **Fumigènes et molotovs** : une fumée éclairée par le soleil et le ciel, avec ses volutes, et qui cache tout ce qui est derrière elle (rayons des projecteurs, pluie, flammes, éclairs compris) ; un feu d'alcool en vrai volume pour le molotov (on tourne autour, les langues passent les unes devant les autres) : film bleu sur toute la surface qui brûle, corps jaune, pointes orange.
- **Ombres des joueurs** au soleil et devant la lumière la plus proche (torche, lanterne) : au soleil comme aux lampes torches, projecteurs et torches ponctuelles, tous les joueurs et PNJ jusqu'à la distance réglée (*Échap → Paramètres graphiques → Lumières → Ombres*, 128 blocs par défaut), même dans une foule ; les onze ou douze plus proches de toi et de ton axe de vue ont leur ombre exacte, les autres une ombre un peu moins fine.
- **Minimap** en haut à droite : tes coéquipiers toujours, les adversaires seulement quand quelqu'un de ton équipe les voit (dans son champ de vision, ni derrière des blocs pleins ni derrière une fumée, l'écran pas tout blanc d'une flash) ; leur dernière position vue reste 3,5 s. Le porteur de la bombe y a un logo de bombe (à sa couleur, ou en rouge chez l'adversaire repéré). Les sites de bombe et la bombe quand ton équipe la connaît.
- **Réseau à 256 Hz** : positions envoyées 256 fois par seconde, prédiction et rembobinage des tirs. À 10 joueurs, l'hôte envoie moins de 1 Mo par seconde.
- **Entraîne-toi contre des bots** : **Solo → Cartes FPS** liste les cartes FPS de ce PC et celles du relais ; *Télécharger* copie une carte du relais dans tes mondes, *Jouer contre des bots* l'ouvre en mode FPS (un de tes mondes garde son mode pour la suite). En mode FPS dans un monde solo, la carte se remplit de bots qui achètent, se battent en équipe, posent et désamorcent la bombe. Trois niveaux : `/bot facile` (par défaut), `/bot normal`, `/bot difficile`, pris tout de suite par ceux de la partie ; `/bot off` pour jouer seul, `/bot` pour les rappeler ou compléter la partie sans la recommencer, `/bot maxbot 2` à `20` pour le nombre de joueurs (toi compris, gardé pour les parties suivantes), `/start` pour la lancer et `/restart` pour la recommencer. Mort, tu vois l'écran du coéquipier que tu regardes comme si c'était toi (sa vie, ses armes, l'arme dans ses mains) ; la touche Utiliser te fait prendre la place du bot regardé, avec sa vie, ses armes et la bombe s'il la porte. En spectateur (`/gm 3`), la partie continue sans toi, et `/gm fps` t'y remet tout de suite. Chaque bot a son skin, dessiné pour lui aux couleurs de son équipe (tenues de terrain côté T, tenue d'intervention côté CT). **Sur un serveur de construction**, un OP en mode FPS lance de même une partie avec des bots (`/start` ou `/bot`) autour de lui (ou sur les zones FPS du monde) : son jeu la mène, les joueurs du serveur en mode FPS (`/gm fps <pseudo>`) y entrent tout seuls et en sortent en changeant de mode ; il la règle (`/bot maxbot`, `/bot facile|normal|difficile`, `/restart`) et la ferme (`/bot off`, ou en quittant le mode FPS). Elle n'est pas classée, et le monde continue autour pour ceux qui construisent.

### Jouer en ligne

Ouvre **Multijoueur → FPS**, choisis une carte et appuie sur **Jouer**.

- **Échelles** (en construction, les lianes se grimpent pareil) : posées contre le mur visé ; un saut te projette en arrière avec la force du saut ; contre l'échelle, on monte en avançant (on descend en regardant vers le bas), avec une demi-seconde pour prendre de la vitesse. À pleine vitesse, ses barreaux s'entendent, pour soi et pour les joueurs proches, dans tous les modes (en construction en ligne aussi) ; accroupi, on grimpe plus lentement et sans bruit. Le saut accroupi (s'accroupir en l'air replie les jambes : on monte à 1,5 bloc) marche aussi dans tous les modes.
- **Couleurs des coéquipiers** : bleu vif, orange vif, vert vif, jaune vif, violet foncé, puis rouge vif, rose flamant rose, blanc, gris clair, marron foncé. À N contre N, seules les N premières servent (les 5 premières en 5 contre 5). Teintes vérifiées pour les daltoniens (protanopie, deutéranopie, tritanopie).
- **Une version, une partie** : on ne joue en ligne qu'avec la dernière version (bouton *Mettre à jour* en bas à gauche de l'accueil) et seulement avec des joueurs de la même version ; une partie commencée se finit dans sa version (ni le jeu ni le lanceur ne se mettent à jour avant la fin, l'accueil l'indique en vert). Une seule partie FPS à la fois par ordinateur.
- **Ceux qui servent restent anonymes** : un garant ou l'hôte d'une région n'est jamais nommé aux joueurs, et les régions passent par les relais d'iroh (son adresse IP n'est pas donnée).
- **Serveurs de construction** : on y revient là où on était (le relais garde la position de chacun), dans une copie à part du monde : ton monde solo n'est jamais touché. *Échap → Télécharger la carte* en garde une copie dans tes mondes solo.
- **Recherche de cartes** : à droite de « Carte », un champ filtre la liste par nom ou auteur.
- **Joue avec tes amis** : le panneau de droite liste tes amis (en ligne en vert) ; un clic invite dans ton **lobby** (jusqu'à 10 joueurs : il doit tenir dans une équipe de la carte, 5 pour un 5 contre 5, 10 pour un 10 contre 10). Celui qui invite en premier est l'hôte du lobby : il lance la partie pour tout le monde, et vous êtes **toujours dans la même équipe**. Clic droit sur un membre : *Exclure* ou *Promouvoir hôte*.

- Chaque carte affiche clairement son format (**5 contre 5**, 2 contre 2, jusqu'à 10 contre 10). **Seules les cartes 5 contre 5 comptent au classement.**
- **Les cartes de la communauté** : toute carte déjà lancée en partie FPS en ligne apparaît dans la liste de tout le monde et se télécharge toute seule. Elle reste disponible tant qu'un joueur qui y a joué l'a encore (son jeu la renvoie au relais si besoin). Tes autres mondes restent privés.
- Pendant l'échauffement, **les autres joueurs restent anonymes** (« Joueur 3 ») : on ne quitte pas la recherche parce qu'on a vu un nom. Les pseudos s'affichent quand la partie commence au complet.
- **Matchmaking équitable** : d'abord un niveau Elo proche de l'équipe déjà formée (moyenne du lobby), puis les nouveaux avec les nouveaux, puis un facteur de confiance proche (le plus bas du lobby). Jamais avec quelqu'un que tu as signalé.
- **Classement** : tes amis d'abord, le **Top mondial** à côté. Elo calculé comme aux échecs sur les parties 5 contre 5 des 10 cartes les plus jouées, avec les statistiques habituelles (K/D, % de tirs à la tête, ADR, MVP).

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
- **Serveurs de construction** : les zones du monde sont validées par des **joueurs d'autres serveurs de construction**, sans lien avec celui-ci (personne ici n'a intérêt à les attaquer). Le jeu de ces joueurs le fait en arrière-plan pendant qu'ils construisent chez eux. S'il n'y en a aucun, ce sont les joueurs du serveur lui-même.
- **Minimap relevée** : quand une carte est signée pour le FPS, le jeu parcourt en tâche de fond tout ce qu'un joueur peut atteindre depuis les zones de départ (marche, saut, chute, échelle) et garde ce relevé avec la carte. La minimap ne montre plus que ces sols et les murs qui les bornent : aucun décor hors d'atteinte, et un trou dans les murs de la carte s'y voit tout de suite.
- **Inactivité et joueurs manquants** : un joueur en vie qui ne touche à aucune commande pendant 2 minutes de manche (le temps s'ajoute d'une manche à l'autre) sort de la partie, puni comme pour une déconnexion ; il peut la rejoindre. Une équipe à qui il manque un joueur depuis 2 manches reçoit une **compensation** à chaque départ de manche (2 500 $ par absent, partagés entre les présents, annoncés dans le tchat). Elle s'arrête dès qu'il revient : reprise tout de suite s'il revient pendant le temps de départ, à la manche suivante s'il revient en pleine manche. Le menu Échap propose aussi de **voter l'abandon** (toute l'équipe doit dire oui ; l'autre équipe gagne).
- **Fréquentation** : le relais note qui se parle (vocal, tchat d'une partie), joue la même partie FPS ou reste côte à côte en construction. Au-delà d'un seuil (quelques minutes de vocal, une partie ensemble), deux joueurs **ne peuvent plus être garants l'un pour l'autre** ; le score s'efface avec le temps (moitié tous les 14 jours). Les connexions lentes ou instables sont aussi écartées des rôles de garant et d'hôte.
- **Vocal de proximité, partout sur la planète, même dans une foule** : en construction, les 256 joueurs les plus proches (à moins de 48 blocs) t'entendent, de plus en plus bas avec la distance, où que tu sois : de l'autre côté d'une frontière de région, de province ou de pays, au fond d'un pays (sans passer par le relais du monde), et pendant qu'une province change d'hôte. Des essais automatiques le vérifient.
- **Construction synchronisée** : chaque bloc posé ou cassé est vérifié par le garant de sa zone, puis envoyé à chacun ; un bloc refusé (par exemple au-delà de 20 par seconde) est annulé chez le joueur. Celui qui arrive ou revient près d'une zone reçoit exactement ce qui lui manque. Les positions partent jusqu'à 60 fois par seconde quand on est peu nombreux (moins quand il y a du monde autour, jamais moins de 10) ; dans une foule, chacun voit les 2 048 joueurs les plus proches : les 32 plus proches à chaque position, les suivants de moins en moins souvent (une sur 4 jusqu'à 128, une sur 16 jusqu'à 512, une sur 64 au-delà), lissés entre deux, et dessinés de plus en plus simplement. Des essais automatiques (joueurs simulés sur un vrai relais : foule, garant ou propriétaire qui s'en va, arrivée en retard) vérifient que tous les mondes restent identiques, case par case.
- **Des milliers de joueurs sur un monde** : un monde très peuplé est découpé en **régions** (32 × 32 chunks). Dès 24 joueurs dans une région, elle est servie par son **propre relais**, que fait tourner en arrière-plan le jeu d'un joueur **d'un autre monde** (comme les garants FPS : personne n'a intérêt à l'attaquer). Ce relais valide les blocs de sa région, transmet les positions des joueurs proches et renvoie tout au relais du monde, qui en garde la copie : s'il disparaît, la région est reprise sans perte. Le jeu se connecte tout seul aux régions autour de lui (billet signé par le relais du monde), et passe d'une région à l'autre sans coupure.
- **L'hôte le mieux connecté** : chaque jeu mesure sa connexion au relais. Si celle de l'hôte (FPS ou construction) devient mauvaise et qu'un autre joueur est bien mieux connecté, la partie lui passe en cours de route, sans recommencer.
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
| `/fps set maxplayer <n>` | Nombre de joueurs, toujours pair (2 à 20 ; 10 = classé) |
| `/fps set door on\|off` | Portes et trappes utilisables en partie (touche *Utiliser*, avec leur bruit) |
| `/fps set doorsync on\|off` | Double porte : ouvrir un battant ouvre aussi l'autre |
| `/fps info` · `/fps clear …` | Voir ou effacer les réglages |

Les mots peuvent être dans n'importe quel ordre (`/fps set ct spawn` marche aussi). Ta carte est partagée avec tout le monde dès que tu lances une partie dessus.

---

## 🧱 Le mode construction

- **Un monde infini** généré à la volée, avec un cycle jour/nuit, des grottes et des forêts.
- **Survie, créatif, spectateur** (`/gm 0`, `1`, `3`), et **FPS** (`/gm 2`) pour essayer sa carte, même sur un serveur de construction. En survie, un bloc se casse après une demi-seconde de clic tenu, la main frappant à petits coups ; en créatif, tout de suite, d'un petit coup de la main. Le bras pivote depuis l'épaule, qui ne bouge pas : seul le bout descend, avec ce qu'il tient.
- **Arrivée dans un monde** : un écran de chargement tant que le monde autour de toi n'est pas affiché ; tu n'y apparais, chez toi comme chez les autres joueurs (en construction en ligne, en partie FPS), qu'une fois tout chargé.
- **WorldEdit intégré** : plus de 40 commandes `//` (remplir, copier, coller, tourner, sphères, cylindres, annuler…). Sur un serveur de construction en ligne, réservé à ses OP : le relais vérifie et envoie les blocs à tous.
- **L'eau et la lave coulent** : une source ne s'épuise pas, l'eau s'étale sur 7 blocs et la lave sur 3 en allant vers le trou le plus proche, elles tombent en chute ; deux sources d'eau côte à côte en font une troisième ; obsidienne et pierre taillée dès qu'elles se touchent. L'eau avance d'un bloc par quart de seconde et sèche à peu près au même rythme quand on retire sa source (par endroits, au hasard : sa dernière couche, trop mince pour couler, s'en va çà et là), même si tu as quitté le monde entre-temps. Sa **surface descend en pente douce** le long du courant et vers les bords. L'eau **emporte les torches**, la redstone et les fleurs, qui tombent en objets (la lave les brûle).
- **Nager** : 2 blocs par seconde dans l'eau (presque 4 en courant), on y coule doucement, *Saut* fait remonter, *Accroupi* plonger ; contre un bord, on se hisse hors de l'eau, et dans une flaque on saute comme sur la terre ferme. Le **courant pousse** les joueurs et emporte les objets, qui flottent à la surface et descendent les chutes. La lave, elle, ralentit tout et brûle. La **barrière à fluides** (`/give fluid_barrier`) est invisible comme la barrière, mais on la traverse : seuls l'eau et la lave s'y arrêtent.
- **Des blocs qui se posent comme on s'y attend** : escaliers face à toi (à l'envers au plafond), dalles, portes (une double porte s'ouvre en miroir, battant droit à la texture retournée), trappes (elles se relèvent du côté choisi à la pose), vitres qui se raccordent, bannières et portes de deux blocs de haut, bûches couchées contre un mur, pots de fleurs où l'on met (clic droit avec une plante) et enlève (clic droit à nouveau) ses fleurs et plantes.
- **Objets au sol et coffres** : la touche *Lâcher l'objet* (A sur un clavier AZERTY ; avec Ctrl, toute la pile) lance l'objet en main là où tu regardes : vers le haut, en cloche et plus loin ; vers le bas, au sol devant tes pieds. Posé, il flotte en tournant, avec sa vraie forme (un bloc avec ses textures, une plante, une torche allumée) et la lumière de l'endroit, et se ramasse en passant dessus, avec un petit « pop ». Un clic droit ouvre un **coffre de 3 lignes de 10** : clic gauche pour prendre ou poser une pile, clic droit pour un seul objet ; cassé, il laisse tomber son contenu. En ligne, le relais arbitre chaque geste dans l'ordre (un objet quitte l'inventaire avant de partir et n'y entre qu'à la réponse) : deux joueurs qui prennent la même chose n'en obtiennent qu'un, et des essais automatiques (réseau qui perd, rejoue et mélange les messages) vérifient qu'aucun objet n'est jamais doublé.
- **Éditeur de lumières** (touche **L**), d'après le mod *IRLights* : place des **lumières ponctuelles** et des **projecteurs** où tu veux, règle leur couleur, leur intensité, leur portée et leur cône, ajoute des **rayons volumétriques** (poussière, fumée ; ceux des lumières posées sont repris d'une image à l'autre et préparés un peu au-delà des bords de l'écran : ils restent nets quand tu tournes la vue), des **ombres douces** calculées sur les blocs (et les joueurs) et des **gobos** (motifs projetés : fenêtre, feuillage, vitrail… ou tes propres PNG du dossier `gobos`). Une lumière cachée dans un bloc de feuilles ou de verre en sort par ses trous, en rayons ; dans une plante, elle en sort toute ; seul un bloc plein l'enferme. Les flèches et les guides se tirent à la souris dans le monde, la **caméra libre** (F) se promène sans bouger ton personnage. Les lumières font partie de la carte : gardées avec le monde (`lumieres.json`), elles partent avec lui quand tu le partages (la signature de la carte les compte) ; **sur un serveur de construction, les OP les posent pour tout le monde** (un projecteur posé, réglé, déplacé ou retiré par un OP l'est chez chaque joueur ; ceux qui arrivent le reçoivent en s'approchant), et seuls les OP peuvent y toucher. Seules les lumières proches, dans des chunks chargés, s'allument : la liste de l'éditeur montre celles-là, avec leur distance (jusqu'à 1 024 lumières par monde). Leurs réglages communs (qualité, rayons, ombres, surfaces) sont dans *Échap → Paramètres graphiques → Lumières*. En option, des **lumières automatiques** s'allument sur les torches et blocs lumineux proches (expérimental). Pas en mode FPS ; en construction en ligne, réservé aux OP du serveur. Le réglage graphique **Torches ponctuelles** (désactivé de base, pour tout le monde hors partie FPS) s'en sert pour remplacer, autour de toi, la lumière des blocs : chaque torche ou bloc lumineux éclaire comme une lumière ponctuelle de l'éditeur, les murs l'arrêtent, la torche que tu tiens aussi ; plus loin, la lumière des blocs reprend.
- **Lampe torche** (inventaire créatif, ou `/give lampe`) : un vrai projecteur dans ta main, dont la lumière part de l'ampoule et vise le centre de l'écran, que les murs arrêtent et qui trace ses rayons dans l'air selon les réglages des lumières. Elle arrive éteinte dans ta main ; **clic molette** pour l'allumer ou l'éteindre. Allumée, la **molette** règle son faisceau, du plus serré (loin et fort) au plus large (court et doux), et ton réglage est gardé. À la troisième personne et dans les miroirs, tu la tiens, bras tendu, et son verre brille. En construction en ligne, les autres te la voient en main, et sa lumière les éclaire (les huit lampes allumées les plus proches, jusqu'à 96 blocs).
- **Ce que tu tiens, vu de toi comme des autres** : en construction, à la première personne, ton bras et l'objet tenu (bloc, tête, torche, baguette, écran, lampe torche, Foudre de Zeus) sont ceux de ton personnage, à la même place qu'en F5, dans les miroirs et chez les autres joueurs. Le bras est tendu, pointé vers le milieu de l'écran avec la lampe torche, à mi-chemin du milieu et du bas avec le reste, levé devant toi avec la Foudre de Zeus ; un bloc tenu a sa vraie taille.
- **Météo** (hors cartes FPS) : la pluie et l'orage arrivent et repartent tout seuls (une averse dure de 10 à 20 minutes, puis le temps reste sec de 10 minutes à 2 h 30) ; les nuages s'épaississent, le jour s'assombrit, la pluie tombe (pas sous un toit), puis le tonnerre. La **foudre** tombe pendant l'orage, un éclair toutes les 11 secondes en moyenne autour de toi, de préférence sur les points hauts (règle des paratonnerres : un sommet attire ce qui tomberait à moins de trois fois sa hauteur), éclaire tout le paysage, fait jaillir une gerbe d'étincelles et blesse à 3 blocs (5 points de vie) ; d'autres éclairs courent dans les nuages sans tomber, cachés par eux là où ils y entrent. Au-delà du monde chargé, le sol continue jusqu'à l'horizon et se perd dans la brume, plus proche sous la pluie. Chaque coup de tonnerre sonne un peu plus grave ou plus aigu, et la pluie ne tourne pas en boucle à l'oreille. Leur volume se règle à part (Paramètres → Son → Volume de la météo), et ils se taisent tant que le menu Échap (ou un autre menu) est ouvert par-dessus la partie. En ligne, tous les joueurs d'un monde ont le même temps au même moment. `/weather clear|rain|storm` impose un temps, `/weather fix` le fige ; sur un serveur de construction, c'est un OP qui le fait, pour tous ses joueurs (ceux qui arrivent ensuite aussi).
- **Foudre de Zeus** (inventaire créatif, ou `/give foudre`) : un éclair tenu droit dans le poing, par son milieu, long de 1,26 bloc, de la même lumière que la foudre du ciel (blanche, avec des touches de bleu et de violet) ; il crépite sourdement (un grésillement électrique étouffé qui vient de ta main, jamais deux fois pareil), lance des étincelles et éclaire les murs et les joueurs autour de toi, avec ton ombre entière. Son icône (en diagonale dans sa case) et l'objet posé au sol sont ce même éclair. Clic gauche : la foudre tombe là où tu vises, comme avec `/thor` ; en visant le ciel, elle part de ta main, monte et traverse les nuages. En construction en ligne, les autres te voient la tenir, en reçoivent la lumière et l'entendent crépiter jusqu'à 10 blocs ; la foudre que lance un OP (`/thor` ou l'éclair) tombe chez tous les joueurs proches, le même éclair pour chacun (elle est réservée aux OP du serveur).
- **Construis à plusieurs, sans limite de joueurs** : ouvre ton monde en ligne, il continue même si tu pars. Le créateur est OP et gère son serveur (`/op`, `/ban`, `/kill`, le mode de jeu d'un joueur avec `/gm <mode> <pseudo>`, celui des nouveaux venus avec `/defaultgamemode`). Ses OP nomment aussi des OP, un toutes les 30 s au plus, et pas avant 30 s après l'être devenus (`/op` dit le temps qui reste) ; seul le créateur retire l'OP, et personne ne le lui retire.
- **Importe tes mondes Java** (format Anvil, toutes versions) et tes **packs de textures** : les fichiers d'origine ne sont jamais modifiés. Les décors faits d'**entités d'affichage** (blocs et objets posés, souvent dans des barrières) deviennent de vrais blocs, les **têtes à skin** gardent leur texture (téléchargée une fois depuis les serveurs de textures de Minecraft, puis gardée sur ton PC, calque du dessus compris), les **cadres** accrochés aux murs montrent le bloc qu'ils tiennent (et se cassent). Les blocs récents ont leur propre bloc, avec la texture de ton pack s'il l'a : ardoise des abîmes, tuf, roche noire, boue, cuivre (ses quatre états), basalte, mousse, calcite, coraux, champignons géants, bambou, grenouillères, ateliers, feu de camp, lichen lumineux, nouvelles essences de bois (palétuvier, cerisier, bambou, carmin, biscornu, chêne pâle). Chaque muret garde sa pierre, et les lumières invisibles des cartes éclairent. Lanternes, bougies et échafaudages ont leur équivalent.

---

## 👤 Ton compte

- **Pseudo + mot de passe**, rien d'autre. Ton mot de passe ne quitte jamais ton PC.
- **Double authentification** facultative (Authy, Aegis, Google Authenticator, 2FAS, Bitwarden), demandée **une seule fois par appareil**.
- **Clés d'accès** (Windows 10 1903 ou plus récent) : dans **Compte**, *Ajouter une clé d'accès* ; ensuite *Se connecter* → *Avec une clé d'accès* : Windows Hello (visage, empreinte ou code PIN de ce PC), une clé de sécurité (USB, NFC) ou ton téléphone, sans pseudo ni mot de passe à taper, et sans code de double authentification (la clé vérifie déjà que c'est toi). Avec Windows Hello, la clé reste dans ce PC : ni compte Microsoft, ni compte Apple ne sont nécessaires. Le relais ne garde que sa partie publique et un coffre qu'il ne peut pas ouvrir. Une clé d'accès vaut pour le relais où tu l'as ajoutée.
- **Connexion automatique** au lancement, sans retaper ton mot de passe ni ton code.
- **Un pseudo est unique** sur tous les relais, inscrit dans un registre public signé.
- **Amis** : un module à droite de l'accueil et du menu du jeu (Échap), comme celui du lobby FPS : la recherche filtre tes amis, *+* en ajoute un. On est amis dans les deux sens : ajouter quelqu'un lui envoie une **demande d'ami** (notification, *Accepter* ou *Refuser*), et tant qu'il ne l'a pas acceptée, tu ne sais rien de lui. Tu vois où sont tes amis (au menu, dans quel monde de construction, en partie FPS) ; un clic sur un ami le **rejoint** dans son monde (en quittant le tien) ou, si tu es dans le tien, l'y **invite** ; clic droit : rejoindre, inviter, retirer. L'invitation arrive en notification (hors ligne : à sa connexion) et l'attend dans son module, sans limite de temps, jusqu'à ce qu'il l'**accepte** (il rejoint ton monde), la **refuse** (tu le sais) ou l'**ignore** (tu n'en sais rien) ; ton monde fermé, il ne peut l'accepter qu'à sa réouverture. Le statut te rend **invisible** (hors ligne pour tes amis, sans invitations). Tu les retrouves aussi dans le classement.
- **Ton skin** : dans **Paramètres**, un aperçu en 3D de ton personnage et le bouton **Changer de skin** (PNG de 64 × 64 au format des jeux de blocs, posé sur le modèle aux proportions réalistes du jeu : joueurs, bots et PNJ). Les autres joueurs le voient ; il change en direct en construction, et reste le même pendant une partie FPS. Pendant l'échauffement, tout le monde garde le modèle par défaut (anonymat).
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
| Maj gauche | S'accroupir / descendre | | Clic molette | Copier le bloc visé (lampe torche en main : l'allumer ou l'éteindre) |
| Ctrl gauche | Courir | | 1 à 0, molette | Barre d'objets |
| E | Inventaire | | T / `/` | Tchat / commande |
| C (maintenu) | Zoom | | F5 | Vue 1re / 3e personne |
| F2 | Capture d'écran | | F3 | Écran de debug (dont la carte qui dessine, et l'écran : branché sur elle ou sur une autre) |
| L | Éditeur de lumières | | F (éditeur ouvert) | Caméra libre |
| V (maintenu) | Parler (micro, en ligne) | | F1 | Masquer l'interface (il reste le jeu et les pseudos) |

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
| Tab | Scores | | K (maintenu) | Parler (micro) |
| F5 | Capture d'écran | | F1 / F2 | Voter oui / non |

</details>

<details>
<summary><b>Commandes du tchat</b></summary>

Le tchat se navigue avec ← → (Début, Fin, Suppr) ; un clic dans la saisie y place le curseur, glisser (ou Maj + flèches, Ctrl+A) sélectionne, Ctrl+C / Ctrl+X / Ctrl+V copient, coupent et collent. Tchat ouvert, la molette remonte dans les messages, et glisser dessus en sélectionne le texte (Ctrl+C pour le copier). Partout où une commande attend oui ou non, au choix : `on`/`off`, `true`/`false`, `1`/`0` ou `oui`/`non`.

| Commande | Effet |
|----------|-------|
| `/help` | Toutes les commandes |
| `/gm <0-3> [pseudo]` | Mode de jeu (`survival`, `creative`, `fps`, `spectator` marchent aussi) ; celui d'un autre joueur : OP d'un serveur de construction |
| `/defaultgamemode <mode\|none>` | Mode de jeu des nouveaux venus sur un serveur de construction (OP) ; `none` : plus de mode imposé |
| `/kill [pseudo]` | Mourir, ou tuer un joueur (OP d'un serveur de construction) |
| `/tp <x> <y> <z>` · `/tp <pseudo>` · `/tp <pseudo> <x> <y> <z>` · `/tp <pseudo> <pseudo2>` | Se téléporter (« ~ » : par rapport à soi, `~ ~10 ~`), ou téléporter un joueur ; en solo, soi seulement ; sur un serveur de construction, OP seulement |
| `/setspawn` | Le point d'apparition là où tu es : gardé avec le monde en solo ; sur un serveur de construction (OP), là où arrivent les nouveaux venus |
| `/report <pseudo>` · `/unreport <pseudo>` | Signaler un joueur de ta partie, ou annuler |
| `/ami <pseudo>` · `/ami retirer <pseudo>` · `/amis` | Amis (aussi `/friend add <pseudo>`) ; tout le reste dans le module d'amis (accueil, Échap) |
| `/equipe ct` · `/equipe t` | Changer d'équipe pendant l'échauffement (aussi `/team`) |
| `/op` · `/deop` · `/ban` · `/unban` | Gérer son serveur de construction |
| `/timeset <heure\|default>` · `/timefix on\|off` | Régler / arrêter l'heure (`11`, `11.20`, `11:20`, `11h20` ; `default` : soleil de Dust2, immobile ; OP en ligne) |
| `/fps set time [heure\|default]` | Heure fixe d'une carte FPS (par défaut le soleil de Dust2) |
| `/weather` · `/weather clear\|rain\|storm` · `/weather fix [on\|off]` | Voir, imposer ou figer le temps qu'il fait (aussi `/meteo` ; en ligne, chez soi seulement pour l'instant) |
| `/thor` | La foudre tombe là où tu vises (aussi `/foudre` ; OP en ligne) |
| `/worldname <nom>` | Renommer le monde |
| `/lumieres` · `/lumieres effacer` | Ouvrir l'éditeur de lumières, ou retirer toutes les lumières du monde (en ligne : OP, pour tout le monde) |
| `/npc stresstest on\|off` | Foule d'essai : jusqu'à 4 000 PNJ qui se promènent autour de toi, pour voir si le jeu reste fluide (hors mode FPS ; sur un serveur de construction, OP seulement, et les joueurs proches la voient aussi). Un clic gauche les frappe (ils reculent et s'enfuient) ; ils ont la vie d'un joueur (vingt coups, quatre foudres ou une grande chute les tuent, et ils réapparaissent ailleurs dans la foule) et se déplacent comme lui : ils sautent sur une marche d'un bloc, tombent des rebords, nagent dans l'eau et en ressortent, font demi-tour devant un mur, la lave ou un plafond trop bas. Pistache se cache parmi eux : à toi de le trouver (en ligne, les joueurs proches apprennent qui l'a trouvé) |
| `//…` | WorldEdit (créatif ; en ligne, OP du serveur seulement) |

</details>

---

## 🚀 Installation

**Windows** : télécharge **`minelib-launcher.exe`** (lien en haut de la page) et lance-le. Le lanceur ne contient pas le jeu : il demande à GitHub la dernière version publiée, la télécharge avec une barre de progression, vérifie son empreinte SHA-256, puis lance le jeu. Les fois suivantes, il ne télécharge que s'il y a une nouvelle version (et si GitHub est injoignable, il lance celle déjà installée). Le jeu est rangé dans `%APPDATA%\.minelib\bin`.

Le jeu compare aussi son propre fichier à la version officielle (« fichiers vérifiés », ou « exécutable non officiel » s'il a été modifié ou abîmé : une alerte honnête, pas une protection absolue, ce sont les garants qui empêchent la triche). La ligne en bas à gauche de l'accueil indique la version, par exemple « Minelib v77 - à jour (lanceur) ».

Tes mondes, captures et réglages sont dans `%APPDATA%\.minelib` (Windows) ou `~/.minelib` (Linux). Dépose un monde Java dans `saves/` ou un pack de textures dans `resourcepacks/`.

**PC portable** : le jeu dessine avec la carte la plus puissante. Si l'écran est branché sur la partie graphique du processeur, chaque image y est recopiée (FPS plafonnés) : l'écran de debug (F3) le dit, et un écran externe branché sur la prise de la carte dédiée prend le relais tout seul. Pour moins de chaleur et de batterie, *Échap → Paramètres graphiques → Carte graphique : partie intégrée* (au prochain lancement ; seulement sur une machine qui a plusieurs cartes).

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
├── render/     rendu wgpu : chunks, ciel, interface, miroirs, raytracing, armes
├── world/      blocs, génération, lumière, maillage
├── assets/     dossier .minelib, options, packs de textures
├── net/        protocole, chiffrement (Noise), client du relais
├── relay/      relais : comptes, classement, cartes, parties, arbitrage
├── p2p/        découverte des relais (DHT) et tunnels (iroh)
└── app/        le jeu : menus, modes, FPS, WorldEdit, réseau de partie
```

### Pièges connus

- **Ne jamais retrouver une direction ou une position du monde avec l'inverse de la
  vue-projection** (`inv_view_proj`, `view_proj.inverse()`). La projection n'a pas de plan
  lointain (Z inversé, à l'infini) et les coordonnées du monde sont grandes : les arrondis
  de l'inversion et de la soustraction faussent la direction, et l'image tourne ou saute
  quand on bouge la vue. C'est arrivé au ciel (v108), puis aux faisceaux des projecteurs
  (v175 : 0,25° d'erreur à 1 000 blocs de l'origine, 2,4° à 10 000). Il faut partir des
  lignes de la matrice : `view_ray` et `eye_relative` (crate `render`), `dl_view_ray` et
  `dl_view_offset` (`dynlights.wgsl`), `sky_dir` (`world.wgsl`). Le test
  `nothing_rebuilds_the_view_with_the_inverse_matrix` refuse tout retour en arrière.
- **Une image à part s'écrit en entier** (`ColorWrites::ALL`). Seule l'image du monde, où
  l'on ajoute la lumière, garde son alpha. De la v151 à la v186, les rayons n'écrivaient
  pas leur alpha, qui portait la profondeur : la recomposition ne savait plus séparer un
  bloc du ciel, d'où des bords sombres en escalier et des feuillages en taches noires
  (corrigé en v193 ; test `side_images_are_written_whole`).
- **Un bloc qui sort de la grille du raytracing n'a pas changé.** Les cartes d'ombre des
  lumières sont calculées dans cette grille autour de la caméra : refaites quand le joueur
  s'éloigne, elles ne voient plus les murs et la lumière passe à travers (v193 : on garde
  la carte d'avant tant que la grille ne voit pas toute la lumière ; tests
  `moving_the_grid_redoes_only_what_enters_it` et `a_far_grid_does_not_cover_the_light`).
- **La passe des translucides (TAA) a trois cibles.** Depuis la v199, l'eau, le verre et la
  glace y laissent leur face dans une troisième image (`taa::WATER_GB_FORMAT`), éclairée
  ensuite par les lumières de l'éditeur (`fs_water*`, `dyn_deferred.wgsl`). Toute pipeline
  dessinée dans cette passe (contour, écrans, fumigènes…) doit la déclarer, avec une
  écriture vide si elle ne s'en sert pas : sinon wgpu la refuse (erreur de validation).
- **Une mesure GPU se demande quand sa passe commence** (`GpuTimer::pass`, juste avant
  `begin_render_pass`). Demandée pour une passe qui n'a finalement pas lieu, elle relit
  l'ancienne mesure restée dans les requêtes : jusqu'à la v201, les bilans montraient des
  « reflets dans l'eau 0,91 ms » figés sans eau en vue, et l'essai de la demi-résolution
  comptait en calcul exact le temps d'« un pixel sur quatre » qui n'avait pas tourné.
- **Tout ce qui fait attendre le jeu se passe avant la lecture des entrées**
  (`Renderer::next_frame`, à la fin de chaque image) : l'attente du pilote (Reflex), la
  file de la carte graphique et l'image de la fenêtre. Une attente placée après la lecture
  du clavier et de la souris s'ajoute à leur retard (jusqu'à la v203 : 2,6 ms en moyenne,
  plus trois images en file). Le jeu borne lui-même la file de la carte (`in_flight`, deux
  images) : wgpu présente quand même quand son attente d'une seconde expire (écran en
  veille), et la chaîne d'images de Windows ne borne alors plus rien — le 19 novembre, la
  souris avait un retard énorme après une longue absence, à 243 images par seconde.
- **L'alpha de l'historique du TAA n'est pas une opacité.** Depuis la v202, il marque où un
  objet bougeait (1, divisé par deux à chaque image) : la résolution y resserre ses bornes
  pour ne pas laisser de traînées derrière les PNJ. La copie à l'écran l'ignore ; toute
  autre lecture de l'historique doit l'ignorer aussi.

- **Le ciel est un bloc de `world.wgsl` recopié ailleurs** (entre `// CIEL : DÉBUT` et
  `// CIEL : FIN`, voir `entity::sky_wgsl`) : le brouillard des blocs, des entités et des
  joueurs va vers le même ciel, nuages, soleil et lune compris (`sky_shade`). Avant la
  v216, un relief effacé par le brouillard prenait la couleur du ciel vide : il cachait
  encore le soleil et les nuages derrière lui. Ce bloc a ses propres uniformes (liaison
  21, `sky::SkyGlobals`) et ne lit de `g` que son début. Les nuages ne sont jamais
  calculés pour l'écran : seulement dans leurs cartes (`sky.rs`), quelques rangées par
  image.
- **La fumée opaque écrit sa profondeur** (v217, `fs_solid` dans smoke.wgsl) : une
  seconde passe, après sa couleur, là où l'on ne voit plus à travers. Tout ce qui est
  dessiné ensuite d'après la profondeur du monde (rayons des projecteurs, pluie, flammes,
  éclairs) reste derrière elle. Avant, la fumée ne s'écrivait nulle part et tout ce qui
  venait après elle passait devant.
- **Le groupe de liaison du monde a 11 images sur 16.** Le jeu demande à la carte les
  limites minimales (16 images lues par étage de shader) ; la passe « moyenne des voisins »
  des lumières en ajoute 4. Il reste une place : une image de plus dans le groupe du monde
  et dans cette passe, et wgpu refuse la pipeline.
- **Ce qui se dessine après le TAA teste lui-même la profondeur.** La pluie, les éclairs
  et les flammes sont des traits fins qui changent à chaque image : le TAA les effaçait
  presque. Ils sont dessinés sur l'image finale, après `taa.present()`, avec
  `g.cur_view_proj` (sans le décalage du TAA), et leur shader compare sa profondeur à
  celle de la scène (`bolts::SceneDepth`, `textureLoad`). Sans TAA, ils passent par le
  test de profondeur ordinaire.

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
- **Éditeur de lumières** : d'après *IRLights* de qualet (MIT). Détail : `assets/CREDITS_IRLights.txt`.
- **Ciel** : atmosphère d'après Sébastien Hillaire, *A Scalable and Production Ready Sky and Atmosphere Rendering Technique* (code de référence d'Epic Games, MIT) et les tables d'Eric Bruneton (BSD). Détail : `assets/CREDITS_Ciel.txt`.
- **Modèles d'armes** : *Ultimate Gun Pack* de [Quaternius](https://quaternius.com) (CC0).
- **Sons** : bibliothèques libres (CC0). Détail : `crates/app/assets/sounds/CREDITS.txt`.
- **Bibliothèques** : wgpu, winit, rodio, mainline, iroh et d'autres, sous licences MIT / Apache-2.0.

Minelib est un projet indépendant, sans lien avec Mojang, Microsoft ou Valve. Minecraft et Counter-Strike sont des marques de leurs propriétaires respectifs.
