// Minelib (v329) : donne aux jeux des identifiants TURN. Voir LISEZMOI.md.
//
// (v334) Tous les services configurés partent, dans cet ordre : FIXE_*, FIXE2_*, COTURN_*,
// puis Cloudflare. Le jeu (v334 et suivantes) prend le premier qui lui donne une adresse :
// un service épuisé (quota du mois) ou en panne laisse la place au suivant, sans mise à
// jour du jeu. (Les jeux d'avant ne prennent que le premier.)
//
// Les façons, selon les secrets du Worker (Paramètres → Variables et secrets) :
//
// 1. Identifiants fixes d'un service TURN gratuit (ExpressTURN : 1 000 Go par mois, sans
//    carte) : ils ne sont pas écrits dans le dépôt public, seulement ici.
//   FIXE_URLS         adresses TURN, séparées par des virgules
//                     (par exemple « turn:relay1.expressturn.com:3478 »)
//   FIXE_UTILISATEUR  nom d'utilisateur donné par le service
//   FIXE_MOT_DE_PASSE son mot de passe
//   FIXE2_URLS, FIXE2_UTILISATEUR, FIXE2_MOT_DE_PASSE : un deuxième service (ou un
//                     deuxième compte), en secours du premier.
//
// 1 bis. Un serveur coturn à soi (voir deploiement/turn-vps) : identifiants qui expirent,
//    calculés ici avec le secret partagé (« use-auth-secret » de coturn) ; le secret ne
//    quitte jamais ce Worker.
//   COTURN_URLS       adresses TURN du serveur, séparées par des virgules
//                     (« turn:<adresse>:3478 »)
//   COTURN_SECRET     le même que « static-auth-secret » dans turnserver.conf
//
// 2. Cloudflare Realtime TURN (1 000 Go gratuits par mois, mais une carte bancaire est
//    demandée à l'activation, et ce qui dépasse est facturé). La clé TURN ne doit jamais
//    être dans le jeu : ce Worker la garde, et ne donne que des identifiants qui expirent
//    (12 heures). Le jeu en redemande avant la fin.
//   TURN_KEY_ID       identifiant de la clé TURN (Realtime → TURN Server → Create)
//   TURN_KEY_TOKEN    son jeton d'API, montré une seule fois à la création
// Facultatifs, pour ne jamais dépasser l'offre gratuite de Cloudflare :
//   ACCOUNT_ID        identifiant du compte Cloudflare (dans l'adresse du tableau de bord)
//   ANALYTICS_TOKEN   jeton d'API avec la permission « Account Analytics : Read »
//   PLAFOND_GO        Go envoyés dans le mois au-delà desquels plus aucun identifiant n'est
//                     donné (900 par défaut) : les jeux passent alors par les autres chemins.

const TTL = 43200;
const CF = "https://rtc.live.cloudflare.com/v1/turn/keys";
const GRAPHQL = "https://api.cloudflare.com/client/v4/graphql";
// Le décompte du mois, gardé 10 minutes par ce Worker.
const VU_POUR = 600_000;
let vu = { a: 0, octets: 0 };

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
  });
}

// Octets envoyés par le serveur TURN ce mois-ci (null : pas de décompte configuré, ou
// réponse illisible).
async function envoyesCeMois(env, maintenant = new Date()) {
  if (!env.ACCOUNT_ID || !env.ANALYTICS_TOKEN) return null;
  if (Date.now() - vu.a < VU_POUR) return vu.octets;
  const debut = new Date(Date.UTC(maintenant.getUTCFullYear(), maintenant.getUTCMonth(), 1));
  const jour = (d) => d.toISOString().slice(0, 10);
  const query = `query($compte: string!, $debut: Date!, $fin: Date!) {
    viewer { accounts(filter: { accountTag: $compte }) {
      callsTurnUsageAdaptiveGroups(filter: { date_geq: $debut, date_leq: $fin }, limit: 1) {
        sum { egressBytes }
      }
    } }
  }`;
  const r = await fetch(GRAPHQL, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.ANALYTICS_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      query,
      variables: { compte: env.ACCOUNT_ID, debut: jour(debut), fin: jour(maintenant) },
    }),
  });
  if (!r.ok) return null;
  const d = await r.json();
  const groupes = d?.data?.viewer?.accounts?.[0]?.callsTurnUsageAdaptiveGroups;
  if (!Array.isArray(groupes)) return null;
  const octets = groupes.reduce((n, g) => n + (g?.sum?.egressBytes ?? 0), 0);
  vu = { a: Date.now(), octets };
  return octets;
}

// Identifiant anonyme d'un jeu (pour repérer dans les statistiques de Cloudflare un abus
// éventuel) : un condensé de son adresse IP, jamais l'adresse elle-même.
async function anonyme(ip) {
  const h = await crypto.subtle.digest("SHA-256", new TextEncoder().encode("minelib:" + ip));
  return [...new Uint8Array(h).slice(0, 8)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function adresses(liste) {
  return (liste || "").split(",").map((u) => u.trim()).filter((u) => u.startsWith("turn"));
}

// Identifiants fixes (FIXE_* ou FIXE2_*), s'il y en a.
function fixes(urls, utilisateur, motDePasse) {
  const u = adresses(urls);
  if (u.length === 0 || !utilisateur || !motDePasse) return null;
  return { urls: u, username: utilisateur, credential: motDePasse };
}

// (v334) Identifiants d'un coturn à soi (« use-auth-secret ») : nom = date d'expiration
// (secondes Unix) et un nom fixe, mot de passe = HMAC-SHA1 du nom par le secret, en base64.
async function coturn(env) {
  const urls = adresses(env.COTURN_URLS);
  if (urls.length === 0 || !env.COTURN_SECRET) return null;
  const username = `${Math.floor(Date.now() / 1000) + TTL}:minelib`;
  const cle = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(env.COTURN_SECRET),
    { name: "HMAC", hash: "SHA-1" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign("HMAC", cle, new TextEncoder().encode(username));
  const credential = btoa(String.fromCharCode(...new Uint8Array(sig)));
  return { urls, username, credential };
}

// Cloudflare Realtime TURN, s'il est configuré et sous son plafond.
async function cloudflare(env, request) {
  if (!env.TURN_KEY_ID || !env.TURN_KEY_TOKEN) return [];
  const plafond = Number(env.PLAFOND_GO || 900) * 1e9;
  const envoyes = await envoyesCeMois(env);
  if (envoyes !== null && envoyes >= plafond) return [];
  const ip = request.headers.get("CF-Connecting-IP") || "";
  const r = await fetch(`${CF}/${env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.TURN_KEY_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ ttl: TTL, customIdentifier: await anonyme(ip) }),
  });
  if (!r.ok) return [];
  const corps = await r.json();
  return corps.iceServers ?? [];
}

export default {
  async fetch(request, env) {
    if (request.method !== "GET") return json({ erreur: "GET seulement" }, 405);
    const groupes = [
      fixes(env.FIXE_URLS, env.FIXE_UTILISATEUR, env.FIXE_MOT_DE_PASSE),
      fixes(env.FIXE2_URLS, env.FIXE2_UTILISATEUR, env.FIXE2_MOT_DE_PASSE),
      await coturn(env),
    ].filter((g) => g !== null);
    groupes.push(...(await cloudflare(env, request)));
    if (groupes.length === 0) {
      return json({ erreur: "aucun service TURN configuré (FIXE_*, FIXE2_*, COTURN_*, TURN_KEY_*)" }, 503);
    }
    // Des identifiants qui expirent (coturn, Cloudflare : 12 heures) fixent la durée
    // annoncée : le jeu en redemande avant. Sinon, un jour.
    const expirent = env.COTURN_SECRET || env.TURN_KEY_ID;
    return json({ iceServers: groupes, ttl: expirent ? TTL : 86400 });
  },
};
