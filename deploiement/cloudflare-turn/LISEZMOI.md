# Minelib par un serveur TURN (0 €, sans carte bancaire)

(v329) L'autre moyen de faire passer les tunnels sans montrer l'adresse IP des joueurs,
sans machine à nous : un service **TURN**.

- Un serveur TURN prête à chaque jeu une adresse chez lui. Quand le joueur et l'hôte d'un
  relais en ont une, le relais du monde donne à chacun celle de l'autre : leurs paquets
  passent par ce serveur, aucun des deux ne voit l'adresse de l'autre.
- Ce que voit le service : l'adresse IP des jeux qui passent par lui, jamais le contenu
  (chiffré de bout en bout). Comme les serveurs d'iroh ou des machines à nous.
- Le jeu préfère toujours les passeurs quand une route en a (ils ne comptent pas dans le
  quota du service) ; TURN prend le relais quand ils se taisent, ou quand il n'y en a pas.

Le jeu prend ses identifiants TURN auprès d'un petit programme chez Cloudflare, un
**Worker** (gratuit, sans carte : 100 000 demandes par jour, un jeu en fait deux ou trois).
C'est son adresse qu'on écrit dans `relais_turn.txt`, et lui seul garde les identifiants :
ils ne sont pas en clair dans le dépôt public.

## Le service retenu : ExpressTURN

1 000 Go par mois gratuits, **sans carte bancaire** : il ne peut rien facturer. Quand le
quota du mois est épuisé (ou si quelqu'un d'autre s'en sert), TURN s'arrête jusqu'au mois
suivant et le jeu passe par ses autres chemins. Ses identifiants sont fixes (un nom et un
mot de passe, dans son tableau de bord) : n'importe qui qui interroge le Worker peut les
lire ; en cas d'abus, en changer dans le tableau de bord d'ExpressTURN, puis dans le Worker.

1. **Compte** sur expressturn.com (e-mail et mot de passe). Dans le tableau de bord :
   les adresses des serveurs (`turn:<serveur>:3478`), le nom d'utilisateur et le mot de
   passe.
2. **Worker** : sur dash.cloudflare.com (un compte sans carte suffit ; un compte à part,
   pour que son sous-domaine `<nom>.workers.dev` ne dise rien d'autre que ce qu'on veut),
   Workers & Pages → Create → Worker, nom `turn` → Deploy, puis Edit code : remplacer tout
   le code par celui de `worker.js` → Deploy.
3. **Secrets du Worker** : Settings → Variables and Secrets → Add, type « Secret » :
   `FIXE_URLS` (les adresses `turn:…`, séparées par des virgules), `FIXE_UTILISATEUR` et
   `FIXE_MOT_DE_PASSE`.
4. **Essai** : ouvrir `https://turn.<sous-domaine>.workers.dev` : une réponse JSON avec
   `iceServers` et `ttl`.
5. **Dans le dépôt** : cette adresse, seule sur une ligne, dans `relais_turn.txt`. Une fois
   la version publiée, chaque jeu et le relais officiel prennent une adresse chez le
   service TURN au lancement.

Pour essayer sans publier : la variable d'environnement `MINELIB_TURN` (l'adresse du
Worker) remplace `relais_turn.txt` pour le jeu ou le relais lancé avec elle.

## Cloudflare Realtime TURN (avec carte)

Le même Worker sait aussi donner des identifiants du TURN de Cloudflare : des points de
présence partout, 1 000 Go gratuits par mois, identifiants de 12 heures. Mais Cloudflare
demande une carte bancaire à l'activation et facture ce qui dépasse (0,05 $ le Go). Le
Worker cesse d'en donner au-delà d'un plafond (900 Go par défaut) si on lui donne de quoi
lire la consommation ; ceux déjà donnés servent jusqu'à leur fin. Secrets : `TURN_KEY_ID`
et `TURN_KEY_TOKEN` (Realtime → TURN Server → créer une clé), et pour le plafond
`ACCOUNT_ID`, `ANALYTICS_TOKEN` (jeton « Account Analytics : Read ») et `PLAFOND_GO`. Sans
les secrets `FIXE_*`, c'est ce mode qui sert.

## Si ça ne suffit plus

Le jeu accepte toute source d'identifiants au format des « iceServers » de WebRTC, et tout
serveur TURN standard (RFC 8656) : par exemple coturn (libre) sur une machine à soi.
Plusieurs lignes dans `relais_turn.txt` : la première qui répond sert.
