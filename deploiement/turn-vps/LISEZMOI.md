# Un serveur TURN à soi, en secours (coturn, sans Docker)

(v334) Le service TURN gratuit (ExpressTURN) donne 1 000 Go par mois. Quand ils sont
épuisés, ou si le service tombe, les joueurs sans passeur repassent par les serveurs relais
d'iroh : 45 Ko/s mesurés, trop peu pour qu'un hôte tienne 128 mises à jour par seconde
pour ses joueurs. Un serveur TURN sur une machine à soi qui a du débit (un VPS au trafic
illimité, par exemple) prend alors la suite, tout seul :

- le Worker (`deploiement/cloudflare-turn`) donne aux jeux tous les services configurés,
  dans l'ordre : ExpressTURN, puis ce serveur ;
- le jeu (v334 et suivantes) prend le premier qui lui donne une adresse. Un service qui
  refuse (quota du mois) ou ne répond pas passe après les autres pendant 10 minutes.

Ce que voit ce serveur : l'adresse IP des jeux qui passent par lui, jamais le contenu
(chiffré de bout en bout). Comme ExpressTURN.

## Installer (Debian ou Ubuntu, en root)

1. Copier ce dossier sur la machine, puis : `sh installer.sh`. Il installe le paquet
   `coturn` de la distribution (pas de Docker), écrit `/etc/turnserver.conf` avec un secret
   tiré au hasard, démarre le service et l'active au démarrage. Relancé, il garde le même
   secret.
2. Pare-feu de l'hébergeur (en plus de `ufw`, que le script ouvre s'il est actif) :
   **3478 en UDP et en TCP**, et **49152 à 65535 en UDP** (les adresses relayées).
3. Le script affiche `COTURN_URLS` et `COTURN_SECRET` : les mettre dans le Worker
   (dash.cloudflare.com → Workers → `turn` → Settings → Variables and Secrets, type
   « Secret »), puis redéployer le Worker avec le `worker.js` de la v334.
4. Essai depuis le PC : le script affiche aussi une commande PowerShell avec des
   identifiants d'un jour pour ce seul serveur (les essais `real_turn` : deux nœuds du PC
   s'y allouent une adresse et s'échangent des paquets, puis la voie rapide FPS par lui).

## Réglages

`turnserver.conf` : `bps-capacity` (débit total que le serveur s'accorde, 100 Mbit/s par
défaut : à régler sur ce que la machine a le droit d'envoyer), `max-bps` (par allocation),
`total-quota` (allocations à la fois). Une partie FPS à 10 joueurs qui passent tous par ce
serveur : environ 4 Mbit/s dans chaque sens. Si la machine est derrière un NAT, la ligne
`external-ip`.

Pour reprendre la main : `systemctl stop coturn` (le jeu repasse par les autres services),
`systemctl disable coturn` pour qu'il ne redémarre pas.
