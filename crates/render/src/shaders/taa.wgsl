// Minelib — anticrénelage temporel (TAA), écrit pour Minelib.
//
// Chaque image est rendue avec un décalage sous-pixel différent. On garde une image
// « historique » : pour chaque pixel on retrouve où il était à l'image précédente (vecteurs
// de mouvement), on y lit l'historique, on le borne aux couleurs voisines de l'image
// courante (évite les traînées) et on mélange un peu de l'image courante. Au fil des
// images, chaque pixel devient la moyenne de nombreux échantillons : plus de scintillement.
// (v202) L'alpha de l'historique marque où un objet bougeait (voir `fs_resolve`).

@group(0) @binding(0) var cur_color: texture_2d<f32>;
@group(0) @binding(1) var cur_motion: texture_2d<f32>;
@group(0) @binding(2) var cur_depth: texture_depth_2d;
@group(0) @binding(3) var history: texture_2d<f32>;
@group(0) @binding(4) var lin_sampler: sampler;
@group(0) @binding(5) var<uniform> params: TaaParams;
@group(0) @binding(6) var prev_depth: texture_depth_2d;

struct TaaParams {
    // xy = taille (px), z = 1 si l'historique est invalide, w = part de l'image courante
    // quand ça bouge
    size: vec4<f32>,
    // x = part de l'image courante pour un pixel immobile (baisse tant que la caméra
    // reste immobile : moyenne de plus en plus longue, image stable)
    still: vec4<f32>,
    // Échelle de rendu : xy = taille de l'image du monde, z = échelle (1 = pleine taille)
    internal: vec4<f32>,
    // xy = décalage sous-pixel de l'image courante (pixels du monde)
    jitter: vec4<f32>,
};

struct FsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> FsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: FsOut;
    o.clip = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return o;
}

fn to_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        0.25 * c.r + 0.5 * c.g + 0.25 * c.b,
        0.5 * c.r - 0.5 * c.b,
        -0.25 * c.r + 0.5 * c.g - 0.25 * c.b,
    );
}

fn from_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}

/// Lecture bicubique (Catmull-Rom) en 5 lectures bilinéaires au lieu de 9 : les 4 coins,
/// de poids presque nul, sont omis et les poids renormalisés. L'historique reste net.
fn sample_history(uv: vec2<f32>) -> vec3<f32> {
    let size = params.size.xy;
    let pos = uv * size;
    let center = floor(pos - 0.5) + 0.5;
    let f = pos - center;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let off12 = w2 / w12;
    let t0 = (center - 1.0) / size;
    let t3 = (center + 2.0) / size;
    let t12 = (center + off12) / size;
    let wc = w12.x * w12.y;
    let wl = w0.x * w12.y;
    let wr = w3.x * w12.y;
    let wt = w12.x * w0.y;
    let wb = w12.x * w3.y;
    let sc = textureSampleLevel(history, lin_sampler, vec2<f32>(t12.x, t12.y), 0.0).rgb;
    let sl = textureSampleLevel(history, lin_sampler, vec2<f32>(t0.x, t12.y), 0.0).rgb;
    let sr = textureSampleLevel(history, lin_sampler, vec2<f32>(t3.x, t12.y), 0.0).rgb;
    let st = textureSampleLevel(history, lin_sampler, vec2<f32>(t12.x, t0.y), 0.0).rgb;
    let sb = textureSampleLevel(history, lin_sampler, vec2<f32>(t12.x, t3.y), 0.0).rgb;
    let c = (sc * wc + sl * wl + sr * wr + st * wt + sb * wb) / (wc + wl + wr + wt + wb);
    // Sans rebond : les lobes négatifs de Catmull-Rom dépassent de part et d'autre d'un bord
    // net. Repris à chaque image tant que la vue bouge un peu, ce dépassement s'accumulait en
    // liserés sombres autour des blocs, des étoiles et des trous du feuillage. Borné aux
    // lectures elles-mêmes (des moyennes des pixels voisins), il reste net sans liseré.
    let lo = min(min(min(sc, sl), min(sr, st)), sb);
    let hi = max(max(max(sc, sl), max(sr, st)), sb);
    return clamp(c, lo, hi);
}

/// Rapproche `c` de `center` jusqu'à ce qu'il entre dans la boîte [lo, hi].
fn clip_box(c: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, center: vec3<f32>) -> vec3<f32> {
    let half = max((hi - lo) * 0.5, vec3<f32>(1e-4));
    let mid = (hi + lo) * 0.5;
    let d = c - mid;
    let t = abs(d / half);
    let m = max(t.x, max(t.y, t.z));
    if m > 1.0 {
        return mid + d / m;
    }
    return c;
}

@fragment
fn fs_resolve(i: FsOut) -> @location(0) vec4<f32> {
    // Pixels du monde (plus petit que l'écran si l'échelle de rendu est réduite).
    let size = vec2<i32>(params.internal.xy);
    let upscale = params.internal.z < 0.999;
    let p = select(vec2<i32>(i.clip.xy), clamp(vec2<i32>(i.uv * params.internal.xy), vec2<i32>(0), size - 1), upscale);
    // Échelle réduite : couleur lue entre les pixels du monde, à l'endroit exact de ce pixel
    // d'écran (décalage sous-pixel annulé) ; sinon le pixel lui-même.
    let cur_uv = i.uv + params.jitter.xy / params.internal.xy;
    let cur = select(
        textureLoad(cur_color, p, 0).rgb,
        textureSampleLevel(cur_color, lin_sampler, cur_uv, 0.0).rgb,
        upscale,
    );

    // Voisinage 3×3 : moyenne et écart type (en YCoCg), et le voisin le plus proche de la
    // caméra (Z inversé : profondeur la plus grande) dont on prend le mouvement — les bords
    // des objets proches suivent ainsi l'objet et non le fond.
    var m1 = vec3<f32>(0.0);
    var m2 = vec3<f32>(0.0);
    var cmin = vec3<f32>(1e9);
    var cmax = vec3<f32>(-1e9);
    var best_depth = -1.0;
    var best = p;
    // (v202) Plus grand mouvement du voisinage (px², au carré) : un objet qui bouge tout près.
    var mmax = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let q = clamp(p + vec2<i32>(dx, dy), vec2<i32>(0), size - 1);
            let c = to_ycocg(textureLoad(cur_color, q, 0).rgb);
            m1 += c;
            m2 += c * c;
            cmin = min(cmin, c);
            cmax = max(cmax, c);
            let d = textureLoad(cur_depth, q, 0);
            if d > best_depth {
                best_depth = d;
                best = q;
            }
            let mv = textureLoad(cur_motion, q, 0).xy;
            mmax = max(mmax, dot(mv, mv));
        }
    }
    let mean = m1 / 9.0;
    let sigma = sqrt(max(m2 / 9.0 - mean * mean, vec3<f32>(0.0)));
    let motion = textureLoad(cur_motion, best, 0).xy;
    let prev_uv = i.uv + motion / params.internal.xy;
    // (v202) Un objet bouge tout près (joueur, PNJ qui marche, bras qui balance).
    let near_motion = mmax > 1e-4;

    let offscreen = any(prev_uv < vec2<f32>(0.0)) || any(prev_uv > vec2<f32>(1.0));
    if params.size.z > 0.5 || offscreen {
        return vec4<f32>(cur, select(0.0, 1.0, near_motion));
    }

    // Pixel immobile (caméra arrêtée) : l'historique est exact, seul le décalage sous-pixel
    // change d'une image à l'autre. On l'accepte alors largement (boîte min/max des voisins,
    // mélange lent) pour que la moyenne converge sans scintiller. En mouvement, boîte plus
    // serrée (écart type) et rejet plus rapide, pour éviter les traînées.
    let speed = length(motion);
    // still.y = 1 : la scène vient de changer (chunk chargé, bloc posé) : comme en mouvement.
    let moving = max(smoothstep(0.05, 1.0, speed), params.still.y);
    let hist_raw = to_ycocg(sample_history(prev_uv));
    // (v202) Alpha de l'historique : 1 là où un objet bougeait à l'image d'avant (et sur
    // un pixel autour), divisé par deux à chaque image. Un PNJ qui marche avance de plus
    // d'un pixel par image : là où il était, plus rien ne bouge, mais l'historique a
    // encore sa couleur.
    let was_moving = textureSampleLevel(history, lin_sampler, prev_uv, 0.0).a;
    let flag = max(select(0.0, 1.0, near_motion), was_moving * 0.5);
    let recent = near_motion || was_moving > 0.2;
    // Immobile : la géométrie a-t-elle changé ici (bloc posé, chunk chargé) ? On compare la
    // profondeur courante à celles de l'image précédente autour du pixel. Si rien n'a
    // changé, l'historique est bon : on ne le borne que très largement (les détails plus
    // fins qu'un pixel, vus seulement par certains décalages, ne clignotent plus).
    let d_cur = textureLoad(cur_depth, p, 0);
    // En croix (5 lectures au lieu de 9) : suffit à reconnaître la même géométrie.
    var pmin = 1e9;
    var pmax = -1e9;
    for (var k = 0; k < 5; k++) {
        let o = select(select(select(select(vec2<i32>(0, 0), vec2<i32>(1, 0), k == 1), vec2<i32>(-1, 0), k == 2), vec2<i32>(0, 1), k == 3), vec2<i32>(0, -1), k == 4);
        let q = clamp(p + o, vec2<i32>(0), size - 1);
        let d = textureLoad(prev_depth, q, 0);
        pmin = min(pmin, d);
        pmax = max(pmax, d);
    }
    // (v202) Un objet bouge ou bougeait tout près (`recent`) alors que ce pixel-ci est
    // immobile (caméra arrêtée) : ce qu'il y avait ici à l'image d'avant a pu partir. La
    // croix des profondeurs d'avant (qui recouvre l'objet et le fond à son bord) le
    // prendrait pour la même géométrie et garderait sa couleur : une traînée derrière
    // chaque membre, des contours laissés partout où les PNJ sont passés. Ici, seule la
    // profondeur de ce pixel-ci compte ; si elle a changé, l'historique est celui d'autre
    // chose : l'image courante seule.
    let mv_here = textureLoad(cur_motion, p, 0).xy;
    let still_here = dot(mv_here, mv_here) < 1e-4;
    let d_prev = textureLoad(prev_depth, p, 0);
    let changed_here = abs(d_prev - d_cur) > d_cur * 0.02 + 1e-7;
    if recent && still_here && changed_here {
        return vec4<f32>(cur, flag);
    }
    let same_geometry = select(
        d_cur >= pmin * 0.98 - 1e-7 && d_cur <= pmax * 1.02 + 1e-7,
        !changed_here,
        recent,
    );
    // Près d'un objet qui bouge : bornes serrées et mélange rapide, comme en mouvement.
    let tight = max(moving, select(0.0, 1.0, recent));
    let span = cmax - cmin;
    let margin = select(span * 0.1, span + vec3<f32>(mean.x * 0.5 + 0.02), same_geometry);
    let lo = mix(cmin - margin, max(mean - sigma, cmin), tight);
    let hi = mix(cmax + margin, min(mean + sigma, cmax), tight);
    let hist = clip_box(hist_raw, lo, hi, mean);

    // Historique très corrigé (objet apparu, chunk chargé…) : on reprend l'image courante.
    let rejected = abs(hist_raw.x - hist.x) / max(mean.x, 0.05);
    let base = mix(params.still.x, params.size.w, tight);
    let alpha = clamp(base + speed * 0.01 + rejected * mix(0.5, 2.0, tight), base, 0.8);
    let out = mix(hist, to_ycocg(cur), alpha);
    return vec4<f32>(from_ycocg(out), flag);
}

