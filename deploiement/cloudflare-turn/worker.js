// Minelib (v329) : donne aux jeux des identifiants TURN. Voir LISEZMOI.md.
//
// Deux façons, selon les secrets du Worker (Paramètres → Variables et secrets) :
//
// 1. Identifiants fixes d'un service TURN gratuit (ExpressTURN : 1 000 Go par mois, sans
//    carte) : ils ne sont pas écrits dans le dépôt public, seulement ici.
//   FIXE_URLS         adresses TURN, séparées par des virgules
//                     (par exemple « turn:relay1.expressturn.com:3478 »)
//   FIXE_UTILISATEUR  nom d'utilisateur donné par le service
//   FIXE_MOT_DE_PASSE son mot de passe
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

// Identifiants fixes (FIXE_*), s'il y en a.
function fixes(env) {
  if (!env.FIXE_URLS || !env.FIXE_UTILISATEUR || !env.FIXE_MOT_DE_PASSE) return null;
  const urls = env.FIXE_URLS.split(",").map((u) => u.trim()).filter((u) => u.startsWith("turn"));
  if (urls.length === 0) return null;
  return {
    iceServers: [{ urls, username: env.FIXE_UTILISATEUR, credential: env.FIXE_MOT_DE_PASSE }],
    ttl: 86400,
  };
}

export default {
  async fetch(request, env) {
    if (request.method !== "GET") return json({ erreur: "GET seulement" }, 405);
    const f = fixes(env);
    if (f) return json(f);
    if (!env.TURN_KEY_ID || !env.TURN_KEY_TOKEN) {
      return json({ erreur: "ni FIXE_URLS, FIXE_UTILISATEUR et FIXE_MOT_DE_PASSE, ni TURN_KEY_ID et TURN_KEY_TOKEN" }, 500);
    }
    const plafond = Number(env.PLAFOND_GO || 900) * 1e9;
    const envoyes = await envoyesCeMois(env);
    if (envoyes !== null && envoyes >= plafond) {
      return json({ iceServers: [], plein: true }, 503);
    }
    const ip = request.headers.get("CF-Connecting-IP") || "";
    const r = await fetch(`${CF}/${env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.TURN_KEY_TOKEN}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ ttl: TTL, customIdentifier: await anonyme(ip) }),
    });
    if (!r.ok) return json({ erreur: `Cloudflare : ${r.status}` }, 502);
    const corps = await r.json();
    return json({ iceServers: corps.iceServers ?? [], ttl: TTL });
  },
};
