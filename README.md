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
| 🔒 **Ta vie privée respectée** | Un pseudo et un mot de passe, c'est tout. Pas d'e-mail, pas de téléphone, ton adresse IP n'est pas donnée aux joueurs de ta partie, chat vocal chiffré de bout en bout. |
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
- **Ombres des joueurs** au soleil et devant la lumière la plus proche (torche, lanterne) : au soleil comme aux lampes torches, projecteurs et torches ponctuelles, tous les joueurs et PNJ jusqu'à la distance réglée (*Échap → Paramètres graphiques → Lumières → Ombres*, 128 blocs par défaut), même dans une foule ; les neuf plus proches de toi et de ton axe de vue ont leur ombre exacte (chaque morceau du corps tourné avec lui, au soleil comme aux lumières : la lumière passe entre les jambes, même immobile), les autres une ombre un peu moins fine. À la première personne, ton ombre part des pieds que tu vois en baissant les yeux.
- **Minimap** en haut à droite : tes coéquipiers toujours, les adversaires seulement quand quelqu'un de ton équipe les voit (dans son champ de vision, ni derrière des blocs pleins ni derrière une fumée, l'écran pas tout blanc d'une flash) ; leur dernière position vue reste 3,5 s. Le porteur de la bombe y a un logo de bombe (à sa couleur, ou en rouge chez l'adversaire repéré). Les sites de bombe, et la bombe lâchée ou posée : toujours pour les T ; pour les CT, seulement quand l'un d'eux la voit (aux mêmes conditions qu'un adversaire), puis 3,5 s.
- **Réseau à 256 Hz** : positions envoyées 256 fois par seconde, prédiction et rembobinage des tirs. À 10 joueurs, l'hôte envoie moins de 1 Mo par seconde.
- **Voie rapide par des passeurs** *(v270 : essayée sur une seule machine, pas encore avec de vraies box ni de vrais joueurs)* : une fois la partie commencée, les entrées et les positions partent aussi en paquets UDP, portés par des **passeurs** (deux par équipe quand il y en a assez) : les jeux de joueurs sans aucun lien avec la partie, choisis par le relais, dont la box a ouvert un port. Un paquet perdu est remplacé par le suivant, 4 ms après, au lieu de retenir tous ceux qui le suivent. Un passeur voit des adresses IP et des paquets scellés, jamais un pseudo ni une partie ; un joueur ne voit que l'adresse des passeurs de sa propre équipe. Le relais resonde ses passeurs toutes les 30 secondes et retire celui qui ne répond plus. (v274) Chaque bout mesure le temps d'aller-retour par chaque passeur : le plus rapide des deux est pris quand l'écart est net, et l'hôte signale au relais un passeur muet ou plus lent de 30 ms que l'autre, qui est remplacé. Les états complets (vie, arme, tableau de bord) partent eux aussi par les passeurs, en plus du relais. La voie rapide ne fait jamais moins bien que le relais : sans passeur disponible, s'ils se taisent, ou si les paquets arrivent plus tard par eux que par le relais, tout repasse par le relais comme avant, sans rien régler. Être passeur suit le réglage du garant : avec `garant=false` dans `options.txt`, ton jeu ne sert ni de garant ni de passeur et n'ouvre aucun port sur ta box. (v286) Quand la voie rapide est ouverte et pas plus lente que le relais, les entrées d'un joueur ne partent plus par le relais que tous les 4 pas (64 messages par seconde au lieu de 256 à 256 images par seconde) : elles arrivent à l'hôte par les passeurs entre-temps, et le relais porte quatre fois moins de messages par joueur.
- **Entraîne-toi contre des bots** : **Solo → Cartes FPS** liste les cartes FPS de ce PC et celles du relais ; *Télécharger* copie une carte du relais dans tes mondes, *Jouer contre des bots* l'ouvre en mode FPS (un de tes mondes garde son mode pour la suite). En mode FPS dans un monde solo, la carte se remplit de bots qui achètent, se battent en équipe, posent et désamorcent la bombe. Un bot ne te voit que si tu es sur son écran (le champ de vision d'un joueur) et à découvert : ni dans son dos, même tout près, ni à travers un mur ou une fumée. Trois niveaux : `/bot facile` (par défaut), `/bot normal`, `/bot difficile`, pris tout de suite par ceux de la partie ; `/bot off` pour jouer seul, `/bot` pour les rappeler ou compléter la partie sans la recommencer, `/bot maxbot 2` à `20` pour le nombre de joueurs (toi compris, gardé pour les parties suivantes), `/start` pour la lancer et `/restart` pour la recommencer. Mort, tu vois l'écran du coéquipier que tu regardes comme si c'était toi (sa vie, ses armes, l'arme dans ses mains) ; la touche Utiliser te fait prendre la place du bot regardé, avec sa vie, ses armes et la bombe s'il la porte. En spectateur (`/gm 3`), la partie continue sans toi, et `/gm fps` t'y remet tout de suite. Chaque bot a son skin, dessiné pour lui aux couleurs de son équipe (tenues de terrain côté T, tenue d'intervention côté CT). **Sur un serveur de construction**, un OP en mode FPS lance de même une partie avec des bots (`/start` ou `/bot`) autour de lui (ou sur les zones FPS du monde) : son jeu la mène, les joueurs du serveur en mode FPS (`/gm fps <pseudo>`) y entrent tout seuls et en sortent en changeant de mode ; il la règle (`/bot maxbot`, `/bot facile|normal|difficile`, `/restart`) et la ferme (`/bot off`, ou en quittant le mode FPS). Elle n'est pas classée, et le monde continue autour pour ceux qui construisent.

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
- **Construction synchronisée** : chaque bloc posé ou cassé est vérifié par le garant de sa zone, puis envoyé à chacun ; un bloc refusé (par exemple au-delà de 20 par seconde) est annulé chez le joueur. Celui qui arrive ou revient près d'une zone reçoit exactement ce qui lui manque. Les positions partent jusqu'à 256 fois par seconde quand on a 3 voisins ou moins (v275 : 4 joueurs au même endroit ; une position par image au plus, il faut donc que le jeu tourne à 256 images par seconde, et le relais ne les retient plus 20 ms pour les regrouper ; pas quand le relais de la région ou de la province, hébergé par un joueur, n'est joint que par les serveurs relais publics d'iroh, au débit limité), 60 fois au-delà et quand on est seul (moins quand il y a du monde autour, jamais moins de 10). **Dans une foule, tes 3 voisins les plus proches te voient quand même 256 fois par seconde** (v285) : entre deux positions pour tous, le jeu en envoie de plus rapprochées que le relais ne remet qu'à eux, sans les retenir ; chacun reçoit alors jusqu'à 768 positions de plus par seconde (ses 3 voisins proches), et envoie 256 messages par seconde au lieu de 10 à 60. Dans une foule, chacun voit les 2 048 joueurs les plus proches : les 32 plus proches à chaque position, les suivants de moins en moins souvent (une sur 4 jusqu'à 128, une sur 16 jusqu'à 512, une sur 64 au-delà), lissés entre deux, et dessinés de plus en plus simplement. (v288) Quand l'hôte d'un pays tombe, un bloc que deux relais avaient remis (celui qu'on quitte et celui qui sert la zone à sa place) n'est plus défait chez certains joueurs seulement. Des essais automatiques (joueurs simulés sur un vrai relais : foule, garant ou propriétaire qui s'en va, arrivée en retard) vérifient que tous les mondes restent identiques, case par case.
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
| `/fps set door` | La porte visée, ou celles de la sélection de la baguette, s'ouvrent en partie (une sélection d'un seul bloc de porte prend toute la porte, les quatre blocs d'une double porte) ; `/fps clear door [all]` les retire |
| `/fps set door on\|off` | Toutes les portes et trappes utilisables en partie (touche *Utiliser*, avec leur bruit), ou seulement celles posées avec `/fps set door` |
| `/fps set doorsync on\|off` | Double porte : ouvrir un battant ouvre aussi l'autre |
| `/fps showelements on\|off` | Hors partie, voir à travers les murs les portes (vert clair, doubles portes vert foncé), les sites (rouge, avec leur nom) et les départs CT (bleu) et T (jaune), comme une sélection de la baguette ; la sélection qui est l'un d'eux en prend la couleur |
| `/fps info` · `/fps clear …` | Voir ou effacer les réglages |

Les mots peuvent être dans n'importe quel ordre (`/fps set ct spawn` marche aussi). Ta carte est partagée avec tout le monde dès que tu lances une partie dessus.

---

## 🧱 Le mode construction

- **Un monde infini** généré à la volée, avec un cycle jour/nuit, des grottes et des forêts. Loin du centre (40 000 blocs et bien au-delà), l'image reste nette : tout est dessiné depuis la caméra, sans les tremblements, les faces qui disparaissent ni les textures qui grésillent des grandes coordonnées ; et on y marche, s'accroupit ou vole à la même vitesse qu'au centre, même à 300 images par seconde.
- **Survie, créatif, spectateur** (`/gm 0`, `1`, `3`), et **FPS** (`/gm 2`) pour essayer sa carte, même sur un serveur de construction. En survie, un bloc se casse après une demi-seconde de clic tenu, la main frappant à petits coups, et tombe en objet au sol, là où il était (un écran aussi), à ramasser en passant dessus ; en créatif, tout de suite, d'un petit coup de la main. Avec le vol permis (`/fly`), aucune chute ne blesse. Le bras pivote depuis l'épaule, qui ne bouge pas : seul le bout descend, avec ce qu'il tient.
- **Arrivée dans un monde** : un écran de chargement tant que tout n'est pas prêt : chaque chunk jusqu'à ta distance de rendu affiché, les shaders du monde, et en ligne la météo du serveur ; jamais avant, même si c'est long (pas d'entrée forcée au bout d'un temps ; Échap reste possible). De même après une téléportation ou une réapparition là où le monde n'est pas chargé. Tu n'y apparais, chez toi comme chez les autres joueurs (en construction en ligne, en partie FPS), qu'une fois tout chargé. Derrière l'écran, tous les cœurs du processeur chargent le monde et ses maillages partent à la carte graphique par grosses fournées. Sa barre avance au rythme de toute l'attente : les chunks, mais aussi les shaders du monde encore en compilation juste après le lancement du jeu (leur durée aux lancements précédents est gardée dans `durees_shaders.txt`) ; elle n'est pleine qu'à l'entrée.
- **WorldEdit intégré** : plus de 40 commandes `//` (remplir, copier, coller, tourner, sphères, cylindres, annuler…). Sur un serveur de construction en ligne, réservé à ses OP : le relais vérifie et envoie les blocs à tous.
- **L'eau et la lave coulent** : une source ne s'épuise pas, l'eau s'étale sur 7 blocs et la lave sur 3 en allant vers le trou le plus proche, elles tombent en chute ; deux sources d'eau côte à côte en font une troisième ; obsidienne et pierre taillée dès qu'elles se touchent. L'eau avance d'un bloc par quart de seconde et sèche à peu près au même rythme quand on retire sa source (par endroits, au hasard : sa dernière couche, trop mince pour couler, s'en va çà et là ; la lave s'éteint de même), même si tu as quitté le monde entre-temps. Sa **surface descend en pente douce** le long du courant et vers les bords. L'eau **emporte les torches**, la redstone et les fleurs, qui tombent en objets (la lave les brûle).
- **Nager** : 2 blocs par seconde dans l'eau (presque 4 en courant), on y coule doucement, *Saut* fait remonter, *Accroupi* plonger ; contre un bord, on se hisse hors de l'eau, et dans une flaque on saute comme sur la terre ferme. Le **courant pousse** les joueurs et emporte les objets, qui flottent à la surface et descendent les chutes. La lave, elle, ralentit tout et brûle. La **barrière à fluides** (`/give fluid_barrier`) est invisible comme la barrière, mais on la traverse : seuls l'eau et la lave s'y arrêtent.
- **Des blocs qui se posent comme on s'y attend** : escaliers face à toi (à l'envers au plafond), dalles (pour les matériaux de construction : ni TNT, ni eau, ni lave, ni minerai, ni feuillage, ni bloc à fonction comme un four ou un établi ; ceux déjà posés restent), portes (une double porte s'ouvre en miroir, battant droit à la texture retournée), trappes (elles se relèvent du côté choisi à la pose), vitres qui se raccordent (on les vise, les touche et les traverse exactement là où elles sont dessinées : une vitre seule, en croix, arrête sur toute sa croix), bannières et portes de deux blocs de haut, bûches couchées contre un mur, pots de fleurs où l'on met (clic droit avec une plante) et enlève (clic droit à nouveau) ses fleurs et plantes.
- **Objets au sol et coffres** : la touche *Lâcher l'objet* (A sur un clavier AZERTY ; avec Ctrl ou Maj, toute la pile) lance l'objet en main là où tu regardes, avec un petit souffle : vers le haut, en cloche et plus loin ; vers le bas, au sol devant tes pieds. Posé, il flotte en tournant, avec sa vraie forme (un bloc avec ses textures, une plante, une torche allumée) et la lumière de l'endroit, et se ramasse en passant dessus, avec un petit « pop ». Jeté dans la lave, il brûle en grésillant (sauf un objet légendaire, qui flotte dessus). Un clic droit ouvre un **coffre de 3 lignes de 10** : clic gauche pour prendre ou poser une pile, clic droit pour un seul objet ; cassé, il laisse tomber son contenu. En ligne, le relais arbitre chaque geste dans l'ordre (un objet quitte l'inventaire avant de partir et n'y entre qu'à la réponse) : deux joueurs qui prennent la même chose n'en obtiennent qu'un, et des essais automatiques (réseau qui perd, rejoue et mélange les messages) vérifient qu'aucun objet n'est jamais doublé.
- **Couche de neige** (menu créatif) : la neige fine se pose sur un dessus plein et s'empile : un clic droit sur son dessus ajoute une couche, de 1 à 8 (huit font un bloc). Une seule couche se remplace par le bloc posé dessus, et on s'enfonce d'une couche dans la neige. Cassée, la pile rend ses couches (en survie).
- **Tapis** : on marche dessus (un seizième de bloc). Ils s'empilent comme la neige : un clic droit sur le dessus d'un tapis en ajoute un, de n'importe quelle couleur (laine ou mousse), jusqu'à 16 (un bloc), et chaque couche garde sa couleur. Cassée, la pile rend chacun de ses tapis (en survie). Entre deux piles voisines, les côtés cachés ne sont pas dessinés, ni le sol sous les tapis : un champ de piles ne coûte que ses dessus et son pourtour. Sur un serveur, tout le monde voit la hauteur et la couleur du dessus de chaque pile ; les couleurs des couches du dessous partent avec la carte (pas encore celles posées en ligne).
- **Seau** (menu créatif, ou `/give seau`, `/give seau_eau`, `/give seau_lave`) : vide, un clic droit prend la source d'eau ou de lave visée (l'eau qui coule ne se prend pas : le regard la traverse) ; plein, il pose sa source sur la face visée, à travers l'eau. En survie, il se remplit et se vide ; en créatif, le seau vide enlève les sources sans se remplir et le seau plein ne se vide jamais. Chaque fois, un glouglou de bulles, plus grave pour la lave.
- **Inventaire** : une pile tenue glissée sur plusieurs cases, bouton tenu, s'y répartit (clic gauche : à parts égales, le reste en main ; clic droit : un objet par case) ; pendant que tu glisses, chaque case parcourue montre déjà ce qu'elle recevra, en surbrillance verte comme sous la souris, et le curseur ce qui te restera en main ; un double-clic sur une pile la complète avec le même objet pris dans l'inventaire (la barre d'abord, de droite à gauche, puis le sac de bas en haut). En créatif, dans le menu : clic gauche, un seul objet ; clic droit, la pile ; Maj + clic, la pile droit dans la barre ; clic molette sur un objet, du menu ou de ton inventaire, une pile de 64 au bout du curseur (la case garde la sienne) ; un objet tenu relâché sur le menu (même sur une case vide) disparaît.
- **Éditeur de lumières** (touche **L**), d'après le mod *IRLights* : place des **lumières ponctuelles** et des **projecteurs** où tu veux, règle leur couleur, leur intensité, leur portée et leur cône, ajoute des **rayons volumétriques** (poussière, fumée ; ceux des lumières posées sont repris d'une image à l'autre et préparés un peu au-delà des bords de l'écran : ils restent nets quand tu tournes la vue), des **ombres douces** calculées sur les blocs (et les joueurs ; ombres nettes : leur bord a la forme exacte des blocs, comme la tache du verre, même en qualité Basse) et des **gobos** (motifs projetés : fenêtre, feuillage, vitrail… ou tes propres PNG du dossier `gobos`). Une lumière cachée dans un bloc de feuilles ou de verre en sort par ses trous, en rayons ; dans une plante, elle en sort toute ; seul un bloc plein l'enferme. Passée par du verre teinté (bloc ou vitre, et la glace), sa lumière est filtrée comme par un vrai verre coloré, à chaque surface de verre traversée : un verre foncé en laisse passer moins, la lumière en biais est plus filtrée, plusieurs verres se multiplient (jaune puis cyan : vert ; rouge puis bleu : presque rien), et le rayon ne prend la couleur d'un verre qu'après lui (entre deux verres, celle du premier seul) : un vitrail colore le sol. Cela vaut pour les lumières ponctuelles comme pour les projecteurs (une lumière dans un bloc de verre rouge éclaire en rouge). Une lumière très forte garde sa couleur sur ce qu'elle éclaire au lieu de virer au blanc (elle ne blanchit que peu à peu, comme pour l'œil). Au sol, la tache colorée a la forme exacte des blocs de verre et le motif de leur texture (cadres compris), nette et aux bords lissés comme une ombre : la lumière y est suivie bloc par bloc à travers le verre. Ses rayons se voient aussi loin que porte la distance de rendu, lissés le long de leur trajet (nets en travers) : ni bruit à l'arrêt, ni rayon décalé en marchant ou en tournant la vue. Une lumière ponctuelle peut avoir une **boule de lumière** en son centre (curseur de 0 à 3 blocs) : une lueur très vive, tournée vers toi, qui n'éclaire rien (un bloc devant elle la cache, bords lissés et immobiles) ; son curseur **Fondu des bords** rend ses bords transparents en dégradé, de plus en plus vers le centre : elle prend moins de place. Sous la pluie, les gouttes qui traversent un projecteur ou une lumière brillent (surtout face à la lumière), et leurs éclaboussures se voient sur le sol éclairé. Les flèches et les guides se tirent à la souris dans le monde, la **caméra libre** (F) se promène sans bouger ton personnage. Les lumières font partie de la carte : gardées avec le monde (`lumieres.json`), elles partent avec lui quand tu le partages (la signature de la carte les compte) ; **sur un serveur de construction, les OP les posent pour tout le monde** (un projecteur posé, réglé, déplacé ou retiré par un OP l'est chez chaque joueur ; ceux qui arrivent le reçoivent en s'approchant), et seuls les OP peuvent y toucher. Seules les lumières proches, dans des chunks chargés, s'allument : la liste de l'éditeur montre celles-là, avec leur distance (jusqu'à 1 024 lumières par monde). Leurs réglages communs (qualité, rayons, ombres, surfaces) sont dans *Échap → Paramètres graphiques → Lumières*. En option, des **lumières automatiques** s'allument sur les torches et blocs lumineux proches (expérimental). Pas en mode FPS ; en construction en ligne, réservé aux OP du serveur. Le réglage graphique **Torches ponctuelles** (désactivé de base, pour tout le monde hors partie FPS) s'en sert pour remplacer, autour de toi, la lumière des blocs : chaque torche ou bloc lumineux éclaire comme une lumière ponctuelle de l'éditeur, les murs l'arrêtent, la torche que tu tiens aussi ; plus loin, la lumière des blocs reprend.
- **Lampe torche** (inventaire créatif, ou `/give lampe`) : un vrai projecteur dans ta main, dont la lumière part de l'ampoule et vise le centre de l'écran, que les murs arrêtent et qui trace ses rayons dans l'air selon les réglages des lumières. Elle arrive éteinte dans ta main ; **clic molette** pour l'allumer ou l'éteindre. Allumée, la **molette** règle son faisceau, du plus serré (loin et fort) au plus large (court et doux), et ton réglage est gardé. À la troisième personne et dans les miroirs, tu la tiens, bras tendu, et son verre brille. En construction en ligne, les autres te la voient en main, et sa lumière les éclaire (les huit lampes allumées les plus proches, jusqu'à 96 blocs).
- **Ce que tu tiens, vu de toi comme des autres** : en construction, à la première personne, ton bras et l'objet tenu (bloc, tête, torche, baguette, écran, lampe torche, Foudre de Zeus) sont ceux de ton personnage, à la même place qu'en F5, dans les miroirs et chez les autres joueurs. Le bras est tendu, pointé vers le milieu de l'écran avec la lampe torche, à mi-chemin du milieu et du bas avec le reste, levé devant toi avec la Foudre de Zeus ; un bloc tenu a sa vraie taille. La main vide aussi est ton bras, tendu comme s'il tenait un bloc, en F5 et dans les miroirs comme à la première personne ; accroupi, il descend avec toi. Accroupi, ton personnage tient tout entier dans 1,5 bloc de haut, et l'est en un cinquième de seconde ; à la première personne (hors mode FPS), la caméra avance avec sa tête (jamais dans un mur devant toi), et tu vises depuis elle.
- **Météo** (hors cartes FPS) : la pluie et l'orage arrivent et repartent tout seuls (une averse dure de 10 à 20 minutes, puis le temps reste sec de 10 minutes à 2 h 30) ; les nuages s'épaississent, le jour s'assombrit, la pluie tombe (pas sous un toit), puis le tonnerre. La **foudre** tombe pendant l'orage, un éclair toutes les 11 secondes en moyenne autour de toi, de préférence sur les points hauts (règle des paratonnerres : un sommet attire ce qui tomberait à moins de trois fois sa hauteur), éclaire tout le paysage, fait jaillir une gerbe d'étincelles et blesse à 3 blocs (5 points de vie) ; d'autres éclairs courent dans les nuages sans tomber, cachés par eux là où ils y entrent. Au-delà du monde chargé, le sol continue jusqu'à l'horizon et se perd dans la brume, plus proche sous la pluie. Chaque coup de tonnerre sonne un peu plus grave ou plus aigu, et la pluie ne tourne pas en boucle à l'oreille. Leur volume se règle à part (Paramètres → Son → Volume de la météo), et ils se taisent tant que le menu Échap (ou un autre menu) est ouvert par-dessus la partie. En ligne, tous les joueurs d'un monde ont le même temps au même moment. `/weather clear|rain|storm` impose un temps, `/weather fix` le fige ; sur un serveur de construction, c'est un OP qui le fait, pour tous ses joueurs (ceux qui arrivent ensuite aussi).
- **Foudre de Zeus** (inventaire créatif, ou `/give foudre`) : un éclair tenu droit dans le poing, par son milieu, long de 1,26 bloc, de la même lumière que la foudre du ciel (blanche, avec des touches de bleu et de violet) ; il crépite sourdement (un grésillement électrique étouffé qui vient de ta main, jamais deux fois pareil), lance des étincelles et éclaire les murs et les joueurs autour de toi, avec ton ombre entière. Son icône (en diagonale dans sa case) et l'objet posé au sol sont ce même éclair. Clic gauche : la foudre tombe là où tu vises, comme avec `/thor` ; en visant le ciel, elle part de ta main, monte et traverse les nuages. Sans charge, un petit éclair part de sa pointe jusqu'à ce que tu vises (8 blocs au plus), dans une décharge qui claque, rugit et roule, jamais deux fois la même ; quand un éclair, petit ou chargé, touche un joueur, un PNJ ou L'Évidé (qu'il ne blesse pas), un coup sec l'accompagne (le tien en entier, où qu'il tombe ; celui des autres, de plus en plus bas avec la distance). Ce que touche la foudre que tu lances, c'est toi qui l'as frappé (Pistache se trouve aussi à la foudre) ; sur un écran, le clic lance la foudre sans le casser. En solo, elle est **légendaire** : une seule par carte, chargée en tapant le ciel (deux coups pour une charge) et, pendant l'orage, d'une charge toutes les 10 secondes (dix charges au plus) ; rien ne la supprime en survie, et en créatif, supprimée, elle retourne au menu, d'où on la reprend (le clic molette ne prend jamais sa place). En construction en ligne, les autres te voient la tenir, en reçoivent la lumière et l'entendent crépiter jusqu'à 10 blocs ; la foudre que lance un OP (`/thor` ou l'éclair) tombe chez tous les joueurs proches, le même éclair pour chacun (elle est réservée aux OP du serveur).
- **Construis à plusieurs, sans limite de joueurs** : ouvre ton monde en ligne, il continue même si tu pars. Le créateur est OP et gère son serveur (`/op`, `/ban`, `/kill`, le mode de jeu d'un joueur avec `/gm <mode> <pseudo>`, celui des nouveaux venus avec `/defaultgamemode`). Ses OP nomment aussi des OP, un toutes les 30 s au plus, et pas avant 30 s après l'être devenus (`/op` dit le temps qui reste) ; seul le créateur retire l'OP, et personne ne le lui retire. Un monde ouvert en ligne est **fermé par défaut** : seuls toi, tes OP, ta liste blanche (`/whitelist add <pseudo>`) et les amis que tu invites y entrent, et il n'apparaît pas dans la liste des serveurs ; `/whitelist off` l'ouvre à tous, `/whitelist on` le referme.
- **Importe tes mondes Java** (format Anvil, toutes versions) et tes **packs de textures** : les fichiers d'origine ne sont jamais modifiés. Les décors faits d'**entités d'affichage** (blocs et objets posés, souvent dans des barrières) deviennent de vrais blocs, les **têtes à skin** gardent leur texture (téléchargée une fois depuis les serveurs de textures de Minecraft, puis gardée sur ton PC, calque du dessus compris), les **cadres** accrochés aux murs montrent le bloc qu'ils tiennent (et se cassent). Les blocs récents ont leur propre bloc, avec la texture de ton pack s'il l'a : ardoise des abîmes, tuf, roche noire, boue, cuivre (ses quatre états), basalte, mousse, calcite, coraux, champignons géants, bambou, grenouillères, ateliers, feu de camp, lichen lumineux, nouvelles essences de bois (palétuvier, cerisier, bambou, carmin, biscornu, chêne pâle). Chaque muret garde sa pierre, et les lumières invisibles des cartes éclairent. Lanternes, bougies et échafaudages ont leur équivalent.

---

## 👤 Ton compte

- **Pseudo + mot de passe**, rien d'autre. Ton mot de passe ne quitte jamais ton PC.
- **Double authentification** facultative (Authy, Aegis, Google Authenticator, 2FAS, Bitwarden), demandée **une seule fois par appareil**.
- **Clés d'accès** (Windows 10 1903 ou plus récent) : dans **Compte**, *Ajouter une clé d'accès* ; ensuite *Se connecter* → *Avec une clé d'accès* : Windows Hello (visage, empreinte ou code PIN de ce PC), une clé de sécurité (USB, NFC) ou ton téléphone, sans pseudo ni mot de passe à taper, et sans code de double authentification (la clé vérifie déjà que c'est toi). Avec Windows Hello, la clé reste dans ce PC : ni compte Microsoft, ni compte Apple ne sont nécessaires. Le relais ne garde que sa partie publique et un coffre qu'il ne peut pas ouvrir. Une clé d'accès vaut pour le relais où tu l'as ajoutée.
- **Connexion automatique** au lancement, sans retaper ton mot de passe ni ton code.
- **Le relais vérifie les mots de passe sans faire attendre personne** (v276) : avant, chaque connexion d'un compte (une par lancement du jeu) et chaque inscription le figeaient le temps du calcul (une quinzaine de millisecondes), parties en cours comprises ; quelques fausses connexions par seconde suffisaient à le figer. Le calcul se fait maintenant à part, sur la moitié des cœurs au plus ; s'il y en a trop en attente, la connexion est refusée (« réessaie dans quelques secondes ») au lieu de ralentir ceux qui jouent.
- **Un pseudo est unique** sur tous les relais, inscrit dans un registre public signé.
- **Amis** : un module à droite de l'accueil et du menu du jeu (Échap), comme celui du lobby FPS : la recherche filtre tes amis, *+* en ajoute un. On est amis dans les deux sens : ajouter quelqu'un lui envoie une **demande d'ami** (notification, *Accepter* ou *Refuser*), et tant qu'il ne l'a pas acceptée, tu ne sais rien de lui. Tu vois où sont tes amis (au menu, dans quel monde de construction, en partie FPS) ; un clic sur un ami le **rejoint** dans son monde (en quittant le tien) ou, si tu es dans le tien, l'y **invite** ; clic droit : rejoindre, inviter, retirer. L'invitation arrive en notification (hors ligne : à sa connexion) et l'attend dans son module, sans limite de temps, jusqu'à ce qu'il l'**accepte** (il rejoint ton monde), la **refuse** (tu le sais) ou l'**ignore** (tu n'en sais rien) ; ton monde fermé, il ne peut l'accepter qu'à sa réouverture. Le statut te rend **invisible** (hors ligne pour tes amis, sans invitations). Tu les retrouves aussi dans le classement. Un ami demandé qui n'est pas connecté reçoit la demande à sa prochaine connexion ; un pseudo inconnu est dit dans le module, avec celui qui lui ressemble s'il existe (« lside » pour « 1side » : le l minuscule de la police a un pied, il ne ressemble plus au chiffre 1) et un bouton pour l'ajouter.
- **Ton skin** : dans **Paramètres**, un aperçu en 3D de ton personnage et le bouton **Changer de skin** (PNG de 64 × 64 au format des jeux de blocs, posé sur le modèle aux proportions réalistes du jeu : joueurs, bots et PNJ). Les autres joueurs le voient ; il change en direct en construction, et reste le même pendant une partie FPS. Pendant l'échauffement, tout le monde garde le modèle par défaut (anonymat).
- **7 langues** : français, English, español, Deutsch, italiano, português, Nederlands (Paramètres → Langue).
- **Le son suit ta sortie** (Paramètres → Son) : si Windows change de sortie par défaut (casque branché, Bluetooth revenu) ou si la sortie se perd (débranchée, mise en veille, prise par une autre application), le jeu la rouvre tout seul dans la seconde, sans avoir à changer de périphérique puis à le remettre.
- **Paramètres graphiques** sur deux colonnes, qui tiennent dans toute fenêtre : dans une fenêtre étroite, boutons et curseurs rétrécissent (leur texte aussi) ; quand ils dépassent de l'écran, ils défilent (molette, ou barre à droite comme celle de l'inventaire créatif) ; la liste d'amis défile de même.
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
| C (maintenu) | Zoom | | F5 | Vue 1re / 3e personne (de dos, puis de face ; la caméra ne descend jamais à plus de 55° sous tes yeux, ni donc dans ton personnage) |
| F2 | Capture d'écran (Ctrl+C : copiée ; dans les menus et le tchat ouvert à tout moment, en jeu dans la minute) | | F3 | Écran de debug (dont la carte qui dessine, et l'écran : branché sur elle ou sur une autre) |
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
| F5 | Capture d'écran (Ctrl+C : copiée ; dans les menus et le tchat ouvert à tout moment, en jeu dans la minute) | | F1 / F2 | Voter oui / non |

</details>

<details>
<summary><b>Commandes du tchat</b></summary>

Le tchat se navigue avec ← → (Début, Fin, Suppr) ; un clic dans la saisie y place le curseur, glisser (ou Maj + flèches, Ctrl+A) sélectionne, Ctrl+C / Ctrl+X / Ctrl+V copient, coupent et collent. Tchat ouvert, la molette remonte dans les messages, et glisser dessus en sélectionne le texte (Ctrl+C pour le copier). Partout où une commande attend oui ou non, au choix : `on`/`off`, `true`/`false`, `1`/`0` ou `oui`/`non`. En tapant le début d'une commande (`/fps`, `/bot`…), un cadre à gauche de l'écran (les messages passent à droite) montre ses formes, ce que fait chacune, et ce qui est déjà enregistré qui la concerne (points de départ et zones de la carte FPS, bots, mode de jeu, météo, heure, lumières…) : toutes ses lignes, qui défilent à la molette quand elles ne tiennent pas. Tchat ouvert, Ctrl+C sans texte sélectionné copie la dernière capture d'écran. **Tab complète un pseudo** commencé, dans un message comme dans une commande qui en attend un (joueurs présents, tes amis, toi) : s'il y en a plusieurs, leur liste apparaît au-dessus de la saisie, chaque Tab passe au suivant (Maj+Tab : au précédent) et la liste revient au début à la fin ; rien de tapé, Tab les propose tous.

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

- **(v283) On n'entre jamais dans un monde avant que tout soit chargé** (règle de Viendax :
  « ça ne doit pas être possible »). L'écran de chargement (`Session::loading`) ne se ferme
  qu'à un seul endroit, `check_loaded`, et seulement quand `loading_done` le dit : chaque chunk
  de **toute** la distance de rendu affiché, les shaders du monde prêts, en ligne la météo du
  serveur arrivée. Aucune sortie au bout d'un temps. Il revient après une téléportation ou une
  réapparition là où le monde n'est pas chargé (`arrive_elsewhere`). Pour rejoindre plus vite,
  accélérer le chargement (threads de fond, envois au GPU…), jamais ouvrir la porte plus tôt :
  l'essai `nothing_is_entered_before_everything_is_loaded` échoue sinon.
- **Ne jamais retrouver une direction ou une position du monde avec l'inverse de la
  vue-projection** (`inv_view_proj`, `view_proj.inverse()`). La projection n'a pas de plan
  lointain (Z inversé, à l'infini) et les coordonnées du monde sont grandes : les arrondis
  de l'inversion et de la soustraction faussent la direction, et l'image tourne ou saute
  quand on bouge la vue. C'est arrivé au ciel (v108), puis aux faisceaux des projecteurs
  (v175 : 0,25° d'erreur à 1 000 blocs de l'origine, 2,4° à 10 000). Il faut partir des
  lignes de la matrice : `view_ray` et `eye_relative` (crate `render`), `dl_view_ray` et
  `dl_view_offset` (`dynlights.wgsl`), `sky_dir` (`world.wgsl`). Le test
  `nothing_rebuilds_the_view_with_the_inverse_matrix` refuse tout retour en arrière.
- **(v254) Les vue-projections des shaders sont comptées depuis la caméra.** Chaque passe
  reçoit ses matrices sans la translation de sa caméra (`eye_relative`, `relative_to`,
  `shifted`) et multiplie `p − g.cam_pos.xyz` : deux grands nombres voisins qui se
  soustraient d'abord restent exacts, alors qu'une matrice qui porte la translation
  (40 000 en f32, à ± 4 mm) et un point de même grandeur se perdent dans l'arrondi (sommets
  qui tremblent loin du centre du monde). Dérivées, coordonnées de texture et case d'un bloc
  partent de la place depuis la caméra (`rel`, `local_pos` de `world.wgsl`). Une nouvelle
  matrice pour les shaders, ou un nouveau `view_proj * vec4(p, 1.0)`, doit suivre la même
  règle ; le test `camera_relative_matrices_stay_exact_far_from_the_origin` vérifie les
  aides.
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
  test de profondeur ordinaire. Cette profondeur a le décalage sous-pixel de l'image : un
  bord net devant quelque chose de lumineux y saute d'un pixel à chaque image. Les boules de
  lumière (grandes, immobiles) sont donc ajoutées à l'image du monde **avant** le TAA
  (`Bolts::draw_balls`, `vs_ball` de bolt.wgsl), cachées par sa profondeur : jusqu'à la
  v267, un bloc de feuillage devant la boule de sa lumière « vibrait ».
- **Tout ce qu'on voit doit écrire son mouvement, translucides compris.** Depuis la v268,
  l'eau, le verre et la glace écrivent le leur (`translucent_out` de world.wgsl) au lieu de
  garder celui de ce qui est derrière : un amas de verre teinté devant le ciel se reprenait
  avec le mouvement du ciel (nul en se déplaçant) et s'étalait en traînées. Et le TAA ne
  reprend l'image d'avant que là où elle montrait la même surface (`conf` de taa.wgsl :
  profondeurs comparées à la marge de ce que la caméra a parcouru et tourné,
  `Taa::cam_step`, `Taa::cam_turn`), ce que les bornes de couleur ne savent pas faire là
  où les voisins mêlent un feuillage et le ciel.
- **La teinte du verre sur une surface est suivie dans la grille de blocs.** La carte de
  teinte (`fs_tint`) dit seulement s'il y a du verre et d'où ; l'éclairage différé suit
  alors la lumière case par case jusqu'au point éclairé (`dl_glass_exact`,
  dynlights_glass.wgsl, depuis la v268). Les autres shaders des lumières n'ont pas la
  grille : ils prennent dynlights_glass_off.wgsl (`lights::with_dynlights`), l'éclairage
  différé `lights::with_dynlights_glass`. (v278) Elle s'arrête un bloc et demi après la
  dernière surface de verre que voit la carte de teinte.
- **Le bord des ombres nettes est suivi dans la grille de blocs (v278).** Près d'un bord (le
  filtre en tente mêle des texels éclairés et d'autres non), `dl_block_edge` lit les
  distances des bloqueurs de la carte d'ombre autour (4 × 4 texels) et suit la lumière case
  par case un bloc et demi autour d'eux (`dl_block_exact`, mêmes formes que `shadow_march`).
  Hors de la grille : la carte seule. Toute forme de bloc ajoutée à `shadow_march` doit
  l'être aussi à `dl_block_exact` (sinon le bord de son ombre ne suit pas la carte).

### Relais

Le relais met les joueurs en relation, transmet les données des parties, garde les comptes, le classement et les cartes. Le jeu le trouve tout seul (réseau local, puis DHT de BitTorrent, sinon ce PC devient le relais, joignable sans ouvrir de port grâce à iroh). (v279) Dans l'ordre : les **relais officiels** écrits dans `relais_officiels.txt` (compilés dans le jeu : c'est la publication d'une version qui fait foi, il n'y a aucune clé secrète à garder ; la liste est vide pour l'instant), puis les **relais où ton compte s'est déjà connecté**, retenus sur ton PC et joints directement, puis seulement la liste du DHT. Cette liste-là est ouverte à tous, donc falsifiable : n'importe qui peut la réécrire ou la bloquer. Ça ne coupe plus un joueur de son relais ; un joueur tout neuf, lui, n'a qu'elle tant qu'aucun relais officiel n'est inscrit. (v301) Quand ton jeu héberge le relais d'une région, d'une province ou un relais de secours, il reste anonyme : les joueurs servis ne voient pas ton adresse IP, et tu ne vois pas la leur. Leurs tunnels passent par des **passeurs** : des jeux de joueurs volontaires dont la box laisse entrer l'UDP, les mêmes que pour la voie rapide des parties FPS. Le relais du monde donne la route aux deux bouts (deux passeurs quand il y en a assez : si le premier se tait, le tunnel continue par le second en 3 secondes environ, sans se refermer). Le passeur voit deux adresses IP, sans savoir à qui elles sont ni ce qu'il porte (le tunnel est chiffré de bout en bout). Aucun serveur à payer, et pas de débit limité par un tiers : les voisins les plus proches restent à 256 positions par seconde, même dans une région très peuplée. S'il n'y a pas d'autre passeur et que ta propre box est ouverte, ton jeu porte lui-même ses tunnels : aussi vite qu'en direct, les joueurs n'y voient qu'une adresse de passeur comme une autre (toi, tu vois alors les leurs). Les serveurs relais publics d'iroh (débit limité, prévus pour le développement) ne servent plus qu'en dernier recours : quand il n'y a aucun passeur, ou pour un joueur endormi au fond d'un pays, qui n'a plus de connexion au relais du monde pour demander une route. `hote_direct=true` dans `options.txt` pour être joint en direct (les joueurs servis voient alors ton adresse IP et savent que c'est toi l'hôte) ; depuis la v301 ce n'est plus jamais fait sans cette ligne. Le relais confie d'abord les régions aux PC dont la box est ouverte : ils peuvent porter eux-mêmes leurs tunnels quand il n'y a pas d'autre passeur. (v298) **Invitation** : à chaque connexion, le jeu écrit `invitation.txt` à côté de `options.txt`. Donne la ligne qu'il contient à un ami : il la colle dans son `options.txt`, et son jeu rejoint ton relais directement, sans la liste du DHT (un joueur tout neuf ne dépend plus d'elle). `minelib-relay.exe` est le même relais sans le jeu ; les administrateurs (`admins.txt`) peuvent bannir un tricheur avec `/bantriche <pseudo> <preuve>`.

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
