# Le relais Minelib sur une machine Linux, à jour tout seul

`installer.sh` installe `minelib-relay` sur une machine Linux toujours allumée (VPS,
serveur, machine virtuelle), en relais ou en passeur sans joueur. Il faut :

- une machine x86_64 avec systemd ;
- une distribution assez récente (bibliothèque C 2.35 ou plus : Debian 12, Ubuntu 22.04
  et plus récentes) ;
- un accès root.

## Comment il se met à jour

À chaque push qui change le numéro de version (`VERSION` dans `crates/app/src/update.rs`),
GitHub publie la Release `vNN` pour les joueurs, puis y joint le relais compilé pour Linux
(`minelib-relay_vNN_linux-x86_64` et son empreinte `.sha256`).

Sur la machine, la minuterie `minelib-relay-maj` regarde toutes les dix minutes s'il y a
une version plus récente. Si oui :

1. elle la télécharge ;
2. elle vérifie son empreinte SHA-256 ;
3. elle essaie de la lancer sur cette machine ;
4. elle la met en place et relance le relais.

Rien n'est compilé sur la machine. Si quelque chose ne va pas (empreinte fausse,
programme qui ne démarre pas ici), l'ancienne version reste en place.

Le relais suit les versions publiées, pas chaque push : il doit parler le même protocole
que les jeux des joueurs, et eux ne se mettent à jour qu'aux versions publiées.

## Installer

Copier `installer.sh` sur la machine, puis lancer l'une des deux commandes.

**Un relais :**

```sh
sudo sh installer.sh relais --name "Mon relais"
```

Il est joignable sans port ouvert (iroh). Options :

- `--name <nom>` : le nom du relais ;
- `--journaux` : recevoir les journaux réseau des joueurs qui les partagent ;
- `--pow <bits>` : la preuve de travail demandée aux inscriptions ;
- `--no-dht` : ne pas s'inscrire dans le DHT ;
- (v345) `--admin <pseudo>` : ce compte administre le relais (bannir pour triche). L'option
  peut être donnée plusieurs fois.

**Un passeur sans joueur :**

```sh
sudo sh installer.sh passeur --ip <adresse publique> --port 40000
```

Le passeur sert un port UDP par relais, à partir de `--port`. Ouvrir ces ports UDP (par
exemple 40000 à 40015) dans le pare-feu de la machine et dans celui de l'hébergeur.
Options :

- `--ip <adresse>` : l'adresse publique (sinon, celle que voient les serveurs d'iroh) ;
- `--port <n>` : le premier port UDP (sinon, n'importe lesquels, à ouvrir tous) ;
- `--relais "<nœud> <clé> <nom>"` : un relais à servir en plus des relais officiels et de
  ceux du DHT (la ligne d'une invitation). L'option peut être donnée plusieurs fois.
- (v345) `--invitation "<invitation>"` : l'invitation donnée dans le jeu par `/forwarder
  invite` (valable 7 jours, pour 4 passeurs par défaut, `/forwarder invite 8` pour 8). Pour
  l'instant (v346), un seul compte peut la donner : celui de Viendax. Le passeur la donne à
  ce relais à chaque connexion et y devient de confiance : il porte alors aussi les parties
  FPS, à 256 tick. Il n'y a plus rien à faire ensuite (mises à jour, redémarrages, nouvelle
  adresse IP). `/forwarder remove <pseudo>` lui retire sa confiance ; `/forwarder list` les
  montre.
- (v347) `--sans-journaux` : ne pas envoyer le journal réseau (voir plus bas).

**Les deux sur la même machine** (v345) : relancer le script avec l'autre mode. Le relais a
le service `minelib-relay`, le passeur `minelib-passeur`.

Si aucune version pour Linux n'est encore publiée, l'installation attend : la minuterie
installe le relais dans les dix minutes qui suivent sa publication.

## Où sont les choses

| Quoi | Où |
| --- | --- |
| Le programme et sa version | `/opt/minelib-relay` (`version` : le numéro en place) |
| Les données (comptes, clé du nœud) | `/var/lib/minelib-relay` |
| Les services | `minelib-relay` (relais), `minelib-passeur` (passeur) |
| La minuterie de mise à jour | `minelib-relay-maj` |

Le service tourne sous le compte système `minelib`, sans connexion possible.

## Suivre et changer

- **État :** `systemctl status minelib-relay` (ou `minelib-passeur`)
- **Journal :** `journalctl -u minelib-relay -f` (ou `minelib-passeur`)
- **Vérifier tout de suite s'il y a une mise à jour :** `sudo systemctl start minelib-relay-maj`
- **Changer les options :** relancer `installer.sh` avec les nouvelles (les données
  restent).
- **Tout retirer :**

  ```sh
  sudo systemctl disable --now minelib-relay minelib-passeur minelib-relay-maj.timer
  sudo rm -r /opt/minelib-relay /var/lib/minelib-relay /etc/systemd/system/minelib-relay* \
      /etc/systemd/system/minelib-passeur.service
  ```

## Ce que voit cette machine

**Un passeur** voit l'adresse IP d'un bout de chaque route qu'il porte, et celle de
l'autre passeur de la chaîne. Il ne voit jamais le contenu : il est scellé de bout en bout.

**Un relais** voit ce que voit le relais hébergé par un joueur.

**Deux passeurs tenus par la même personne** et mis dans la même chaîne verraient les deux
bouts.

**Le journal réseau** (v347) : un passeur envoie aux relais qui le demandent les mêmes lignes
qu'un jeu dont le joueur a accepté de partager les siennes (« passeur (60 s) : … paquets
portés », « tunnel : … », « réseau (…) »), sans la question : il n'y a pas de joueur. Elles
ne contiennent ni pseudo, ni adresse, ni numéro de nœud. `--sans-journaux` les garde ici.

## À savoir

- Les passeurs sans joueur servent aux liens avec les relais (construction, régions). Les
  parties FPS ne prennent que des passeurs de confiance : au moins 3 parties jouées et peu
  de signalements, ou (v345) invités (`--invitation`). Sans
  invitation, un passeur sans joueur n'y sert donc pas. Quand il y a des passeurs de joueurs
  et des passeurs invités, chaque chaîne prend un de chaque : des machines tenues par les
  mêmes personnes ne voient jamais les deux bouts d'une route.
- La mise à jour fait confiance aux Releases du dépôt, comme les jeux des joueurs.
  L'empreinte vérifie que le fichier téléchargé est bien celui qui a été publié.
