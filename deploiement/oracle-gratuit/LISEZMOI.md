# Minelib sur les machines gratuites d'Oracle Cloud (0 €)

(v327) De quoi faire tourner, sans rien payer :

- **un relais officiel** toujours allumé (comptes, mondes, parties), à écrire dans
  `relais_officiels.txt` ;
- **deux serveurs relais d'iroh à nous**, à écrire dans `relais_iroh.txt` : le chemin de
  secours des connexions (quand il n'y a ni passeur ni lien direct), sans la limite de
  débit de ceux de l'équipe d'iroh (environ 45 Ko/s mesurés) ;
- **quatre passeurs sans joueur** (quatre adresses IP) : deux chaînes complètes de
  passeurs, sans aucun joueur volontaire.

Ce que voient ces machines : l'adresse IP des jeux qui passent par elles, comme les
serveurs de l'équipe d'iroh aujourd'hui ; jamais le contenu (chiffré de bout en bout). Les
joueurs, eux, ne voient l'adresse de personne.

## Ce qu'Oracle donne (offre « Always Free », novembre 2026)

- 2 petites machines AMD (`VM.Standard.E2.1.Micro`) : 1/8 de processeur, 1 Go de mémoire,
  une adresse IP publique et jusqu'à 50 Mbit/s chacune ;
- 2 processeurs Arm et 12 Go de mémoire à partager (`VM.Standard.A1.Flex`) : ici deux
  machines de 1 processeur et 6 Go (avant juin 2026, c'était le double) ;
- 10 To sortants par mois pour le tout (environ 30 Mbit/s en continu ; une partie FPS à
  256 tick prend environ 100 Ko/s par joueur et par saut).

À savoir : Oracle peut arrêter une machine gratuite restée **inactive 7 jours**
(processeur, réseau et, pour les Arm, mémoire tous sous 20 %) ; on la relance depuis la
console. Passer le compte en « paiement à l'usage » l'évite et reste à 0 € tant qu'on ne
sort pas de l'offre gratuite (mettre une alerte de budget à 1 €). Oracle a déjà changé son
offre sans prévenir (les machines Arm divisées par deux en juin 2026).

## Étapes

1. **Compte** : cloud.oracle.com, offre gratuite. La région d'origine ne se change plus :
   en prendre une proche des joueurs (Paris, Marseille, Francfort).
2. **Réseau** : dans la « security list » du sous-réseau public, ajouter en entrée, depuis
   `0.0.0.0/0` : UDP `40000-40019` (passeurs), TCP `80` et `443`, UDP `7842` (serveurs
   relais d'iroh).
3. **Quatre machines**, Ubuntu 24.04, chacune avec une adresse IP publique :
   - `relais` : A1.Flex, 1 OCPU, 6 Go : relais officiel, serveur relais d'iroh, passeur ;
   - `relais2` : A1.Flex, 1 OCPU, 6 Go : serveur relais d'iroh, passeur ;
   - `passeur1`, `passeur2` : E2.1.Micro : passeur.
4. **Deux noms gratuits** chez DuckDNS (duckdns.org) : `<nom>1.duckdns.org` vers l'adresse
   de `relais`, `<nom>2.duckdns.org` vers celle de `relais2` (il faut un nom pour le
   certificat de Let's Encrypt, gratuit lui aussi).
5. **Le code**, tant que `main` n'est pas sur GitHub : sur le PC,
   `git bundle create minelib.bundle main`, puis `scp minelib.bundle ubuntu@<adresse de relais>:`,
   et sur `relais` : `git clone minelib.bundle minelib`.
6. **Construire**, sur `relais` : `cd minelib && ./deploiement/oracle-gratuit/construire.sh`.
   Copier ensuite (scp) `target/x86_64-unknown-linux-gnu/release/minelib-relay` sur
   `passeur1` et `passeur2`, et `target/release/minelib-relay` et `~/.cargo/bin/iroh-relay`
   sur `relais2` (les petites machines AMD n'ont pas assez de mémoire pour construire).
7. **Sur chaque machine** :

   ```
   sudo useradd -r -m -d /var/lib/minelib minelib
   sudo mkdir -p /opt/minelib /etc/minelib
   sudo cp minelib-relay /opt/minelib/
   sudo ./pare-feu.sh           # sur relais et relais2 : sudo ./pare-feu.sh relais
   ```

8. **Relais officiel**, sur `relais` :
   `sudo cp minelib-relais.service /etc/systemd/system/ && sudo systemctl enable --now minelib-relais`,
   puis `journalctl -u minelib-relais | grep "Ligne pour relais_officiels"` : c'est la
   ligne du relais. Garder `/var/lib/minelib/noeud.key` (son identité) et ne la donner à
   personne.
9. **Serveurs relais d'iroh**, sur `relais` et `relais2` : `iroh-relay` dans `/opt/minelib`,
   `iroh-relay.toml` dans `/etc/minelib` (avec le nom DuckDNS de la machine et une adresse
   de contact pour Let's Encrypt), `iroh-relay.service` dans `/etc/systemd/system`, puis
   `sudo systemctl enable --now iroh-relay`. Vérifier : `curl https://<nom>1.duckdns.org/ping`.
10. **Passeurs**, sur les quatre : dans `minelib-passeur.service`, l'adresse IP publique de
    la machine et la ligne du relais ; copier dans `/etc/systemd/system`, puis
    `sudo systemctl enable --now minelib-passeur`.
11. **Dans le dépôt** : la ligne du relais dans `relais_officiels.txt`, et
    `https://<nom>1.duckdns.org` et `https://<nom>2.duckdns.org` dans `relais_iroh.txt`.
    Une fois la version publiée, les jeux joignent le relais officiel directement, passent
    par vos serveurs relais d'iroh au lieu de ceux de l'équipe d'iroh, et ont quatre
    passeurs ; si ces machines tombent, ils font comme avant (passeurs volontaires, serveurs
    de l'équipe d'iroh).

(Les services `minelib-relais` et `minelib-passeur` prennent vos serveurs relais d'iroh
tout de suite, par la ligne `Environment=MINELIB_RELAIS_IROH=…` : y mettre les deux noms.)

Rien n'oblige à tout faire : une seule machine `relais` donne déjà un relais officiel, un
serveur relais d'iroh à nous et un passeur.
