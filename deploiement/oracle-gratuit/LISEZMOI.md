# Minelib sur les machines gratuites d'Oracle Cloud (0 €)

(v327 ; v329 : tout se fait en une fois, par une « pile » de Resource Manager.)

De quoi faire tourner, sans rien payer :

- **un relais officiel** toujours allumé (comptes, mondes, parties), à écrire dans
  `relais_officiels.txt` ;
- **deux serveurs relais d'iroh à nous**, à écrire dans `relais_iroh.txt` : le chemin de
  secours des connexions (quand il n'y a ni passeur, ni TURN, ni lien direct), sans la
  limite de débit de ceux de l'équipe d'iroh (environ 45 Ko/s mesurés) ;
- **quatre passeurs sans joueur** (quatre adresses IP) : deux chaînes complètes de
  passeurs, sans aucun joueur volontaire.

Ce que voient ces machines : l'adresse IP des jeux qui passent par elles, comme les
serveurs de l'équipe d'iroh ou un service TURN ; jamais le contenu (chiffré de bout en bout).
Les joueurs, eux, ne voient l'adresse de personne.

L'autre moyen, sans Oracle et sans carte bancaire : un service TURN gratuit (ExpressTURN,
voir `deploiement/cloudflare-turn`). Les deux se complètent : le jeu prend ce qui répond.

## Ce qu'Oracle donne (offre « Always Free », novembre 2026)

- 2 petites machines AMD (`VM.Standard.E2.1.Micro`) : 1/8 de processeur, 1 Go de mémoire,
  une adresse IP publique et jusqu'à 50 Mbit/s chacune ;
- 2 processeurs Arm et 12 Go de mémoire à partager (`VM.Standard.A1.Flex`) : ici deux
  machines de 1 processeur et 6 Go (avant juin 2026, c'était le double) ;
- 200 Go de disque en tout (50 par machine), 10 To sortants par mois, un seau de 20 Go.

À savoir : Oracle peut arrêter une machine gratuite restée **inactive 7 jours**
(processeur, réseau et, pour les Arm, mémoire tous sous 20 %) ; on la relance depuis la
console (Compute → Instances → Start). Passer le compte en « paiement à l'usage » l'évite
et reste à 0 € tant qu'on ne sort pas de l'offre gratuite (mettre une alerte de budget à
1 €). Oracle a déjà changé son offre sans prévenir (les machines Arm divisées par deux en
juin 2026). Les machines Arm manquent parfois dans une région (« Out of capacity ») : la
pile crée le reste, et il suffit de relancer « Apply » plus tard.

## Étapes

1. **Compte** : cloud.oracle.com, offre gratuite. La région d'origine ne se change plus :
   en prendre une proche des joueurs (Paris, Marseille, Francfort).
2. **Deux noms gratuits** (facultatif, pour les serveurs relais d'iroh) : sur duckdns.org,
   se connecter (par GitHub, par exemple), créer deux noms (`<nom>1` et `<nom>2`), et
   noter le « token » affiché en haut. Let's Encrypt donne leurs certificats ; les
   machines tiennent elles-mêmes les noms à jour.
3. **La pile** : dans le dépôt, `sh deploiement/oracle-gratuit/faire-la-pile.sh` (sous
   Windows, dans Git Bash) fait `target/minelib-oracle.zip` : le réseau, le pare-feu, le
   seau, les quatre machines, et le code du relais de la dernière version enregistrée.
4. **Dans la console d'Oracle** : menu → Developer Services → Resource Manager → Stacks →
   Create stack → « My configuration », « .Zip file » : déposer `minelib-oracle.zip` →
   Next. Remplir le formulaire (nom du relais, noms et jeton DuckDNS ; le reste peut rester
   tel quel) → Next → cocher « Run apply » → Create.
5. **Attendre** (environ 45 minutes) : la première machine Arm construit Minelib et
   les programmes des autres, puis toutes lancent leurs services. Dans la pile, onglet
   « Application information » : le lien « État de la construction » dit où elle en
   est ; le lien « Ligne du relais officiel » donne la ligne une fois le relais lancé.
6. **Dans le dépôt** : cette ligne dans `relais_officiels.txt`, et les adresses données en
   sortie (`https://<nom>1.duckdns.org`, `https://<nom>2.duckdns.org`) dans
   `relais_iroh.txt`. Une fois la version publiée, les jeux joignent le relais officiel,
   passent par vos serveurs relais d'iroh et ont quatre passeurs ; si ces machines
   tombent, ils font comme avant (TURN, passeurs volontaires, serveurs de l'équipe d'iroh).

## Mettre à jour

Refaire `minelib-oracle.zip` (étape 3), puis dans la pile : Edit → Edit stack → déposer le
nouveau zip → Apply. Seul le seau change : les machines le voient dans les dix minutes,
la première reconstruit, les autres prennent leurs nouveaux programmes, et chacune relance
ses services. Les machines elles-mêmes ne sont jamais refaites : le relais garde son
identité (`noeud.key`) et ses comptes.

## Se connecter à une machine

La pile crée une clé SSH si on ne lui en donne pas (sortie `cle_ssh_privee`) :
`ssh -i cle ubuntu@<adresse>`. Journal : `/var/log/minelib.log` ; services :
`minelib-relais`, `iroh-relay`, `minelib-passeur`, `minelib-maj.timer`.

## Tout retirer

Dans la pile : Destroy. Le seau ne part qu'une fois vidé de ce que les machines y ont mis
(Storage → Buckets → `minelib-pile` → supprimer ses objets), puis Destroy de nouveau.
