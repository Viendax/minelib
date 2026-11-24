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
    // reste immobile : moyenne de plus en plus longue, image stable) ; y = 1 : la scène vient
    // de changer ; (v268) z = pas de la caméra depuis l'image d'avant, rapporté au plan proche,
    // w = angle dont elle a tourné (rad)
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
/// (v204) Rend aussi l'alpha de la lecture du milieu (le marqueur des objets en mouvement,
/// voir `fs_resolve`) : une lecture de moins qu'en le relisant à part.
fn sample_history(uv: vec2<f32>) -> vec4<f32> {
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
    let sc4 = textureSampleLevel(history, lin_sampler, vec2<f32>(t12.x, t12.y), 0.0);
    let sc = sc4.rgb;
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
    return vec4<f32>(clamp(c, lo, hi), sc4.a);
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
    // Voisinage 3×3 : moyenne et écart type (en YCoCg), et le voisin le plus proche de la
    // caméra (Z inversé : profondeur la plus grande) dont on prend le mouvement — les bords
    // des objets proches suivent ainsi l'objet et non le fond.
    //
    // (v204) Mêmes valeurs, moins de lectures (43 → 26 par pixel) : les neuf profondeurs
    // viennent de quatre lectures groupées (`textureGather` : les quatre pixels autour d'un
    // coin, tels quels), le pixel du milieu n'est plus relu à part, et le mouvement du
    // voisinage se lit en quatre moyennes de 2 × 2 pixels au lieu de neuf pixels.
    let isize = params.internal.xy;
    let corner = vec2<f32>(p);
    // Ordre de `textureGather` : (x0, y1), (x1, y1), (x1, y0), (x0, y0).
    let g00 = textureGather(cur_depth, lin_sampler, corner / isize);
    let g10 = textureGather(cur_depth, lin_sampler, (corner + vec2<f32>(1.0, 0.0)) / isize);
    let g01 = textureGather(cur_depth, lin_sampler, (corner + vec2<f32>(0.0, 1.0)) / isize);
    let g11 = textureGather(cur_depth, lin_sampler, (corner + vec2<f32>(1.0, 1.0)) / isize);
    // Les neuf profondeurs, ligne par ligne (dy = -1, 0, 1 ; dx = -1, 0, 1).
    var depths = array<f32, 9>(g00.w, g00.z, g10.z, g00.x, g00.y, g10.y, g01.x, g01.y, g11.y);
    let d_cur = depths[4];
    var m1 = vec3<f32>(0.0);
    var m2 = vec3<f32>(0.0);
    var cmin = vec3<f32>(1e9);
    var cmax = vec3<f32>(-1e9);
    // (v268) Le pixel lui-même à profondeur égale (avant : le premier des neuf, celui d'en haut
    // à gauche, dont le mouvement passait à tout un ciel ou un mur plat).
    var best_depth = d_cur;
    var best = p;
    var center = vec3<f32>(0.0);
    for (var k = 0; k < 9; k++) {
        let q = clamp(p + vec2<i32>(k % 3 - 1, k / 3 - 1), vec2<i32>(0), size - 1);
        let rgb = textureLoad(cur_color, q, 0).rgb;
        if k == 4 {
            center = rgb;
        }
        let c = to_ycocg(rgb);
        m1 += c;
        m2 += c * c;
        cmin = min(cmin, c);
        cmax = max(cmax, c);
        let d = depths[k];
        if d > best_depth {
            best_depth = d;
            best = q;
        }
    }
    // Échelle réduite : couleur lue entre les pixels du monde, à l'endroit exact de ce pixel
    // d'écran (décalage sous-pixel annulé) ; sinon le pixel lui-même.
    var cur = center;
    if upscale {
        let cur_uv = i.uv + params.jitter.xy / isize;
        cur = textureSampleLevel(cur_color, lin_sampler, cur_uv, 0.0).rgb;
    }
    // (v202) Un objet bouge tout près (joueur, PNJ qui marche, bras qui balance) : un des
    // neuf pixels bouge de plus d'un centième de pixel. (v204) Lu en quatre moyennes de
    // 2 × 2 pixels, aux quatre coins du pixel : un seul pixel qui bouge de 0,01 y compte
    // pour 0,0025.
    let ma = textureSampleLevel(cur_motion, lin_sampler, corner / isize, 0.0).xy;
    let mb = textureSampleLevel(cur_motion, lin_sampler, (corner + vec2<f32>(1.0, 0.0)) / isize, 0.0).xy;
    let mc = textureSampleLevel(cur_motion, lin_sampler, (corner + vec2<f32>(0.0, 1.0)) / isize, 0.0).xy;
    let md = textureSampleLevel(cur_motion, lin_sampler, (corner + vec2<f32>(1.0, 1.0)) / isize, 0.0).xy;
    let mmax = max(max(dot(ma, ma), dot(mb, mb)), max(dot(mc, mc), dot(md, md)));
    let near_motion = mmax > 6.25e-6;
    let mv_here = textureLoad(cur_motion, p, 0).xy;
    var motion = mv_here;
    // (v268) Le mouvement du voisin le plus proche de la caméra, seulement s'il est à peu près
    // sur la même surface que ce pixel : un pixel nettement derrière lui (le ciel par un trou
    // du feuillage, un mur derrière le bord d'un objet) garde le sien. Avec celui du voisin,
    // il allait chercher l'image d'avant là où était le feuillage, et l'y prenait.
    var d_ref = d_cur;
    if any(best != p) && d_cur >= best_depth * 0.8 {
        motion = textureLoad(cur_motion, best, 0).xy;
        d_ref = best_depth;
    }
    let mean = m1 / 9.0;
    let sigma = sqrt(max(m2 / 9.0 - mean * mean, vec3<f32>(0.0)));
    let prev_uv = i.uv + motion / params.internal.xy;

    let offscreen = any(prev_uv < vec2<f32>(0.0)) || any(prev_uv > vec2<f32>(1.0));
    if params.size.z > 0.5 || offscreen {
        return vec4<f32>(cur, select(0.0, 1.0, near_motion));
    }

    // (v268) « De la rémanence quand on se déplace autour » (un grand amas de feuillage de la
    // carte Nuke, de nuit : des copies de ses blocs, étalées autour de lui) : là où les voisins
    // mêlent des feuilles sombres et le ciel par leurs trous, les bornes de couleur couvrent
    // les deux, et rien n'empêchait l'image d'avant de garder des feuilles là où il n'y avait
    // plus que du ciel (ou l'inverse), image après image. La caméra bougeant, l'image d'avant
    // n'est reprise que là où elle montrait la même surface : la profondeur de l'image d'avant,
    // là où on la lit (quatre pixels, mêlés selon la distance), comparée à celle de ce pixel,
    // à la marge près de ce que la caméra a parcouru (`still.z` : son pas rapporté au plan
    // proche ; `still.w` : l'angle dont elle a tourné). Une autre surface (un trou découvert,
    // le bord qui a glissé) : l'image courante seule. Caméra immobile : rien ne change (les
    // objets qui bougent ont leur traitement plus bas).
    var conf = 1.0;
    if params.still.z > 0.0 || params.still.w > 0.0 {
        let gp = textureGather(prev_depth, lin_sampler, prev_uv);
        let pf = fract(prev_uv * isize - 0.5);
        // Et la pente de la surface ici (d'un pixel au voisin, sur la même surface : écart de
        // moins de moitié) : un sol vu de loin, presque à plat, change de profondeur de plus
        // de 5 % d'un pixel à l'autre ; sans elle, il aurait perdu son lissage en bougeant.
        var slope = 0.0;
        for (var k = 0; k < 9; k++) {
            let r = abs(depths[k] - d_cur) / max(max(depths[k], d_cur), 1e-7);
            if r < 0.5 {
                slope = max(slope, r);
            }
        }
        let tol = 0.05 + slope + params.still.z * d_ref + 1.2 * params.still.w;
        // Ordre de `textureGather` : (x0, y1), (x1, y1), (x1, y0), (x0, y0).
        let w4 = vec4<f32>((1.0 - pf.x) * pf.y, pf.x * pf.y, pf.x * (1.0 - pf.y), (1.0 - pf.x) * (1.0 - pf.y));
        let s4 = vec4<f32>(
            same_surface(gp.x, d_ref, tol),
            same_surface(gp.y, d_ref, tol),
            same_surface(gp.z, d_ref, tol),
            same_surface(gp.w, d_ref, tol),
        );
        // Un quart des pixels lus sur la même surface suffit (un bord d'objet, que le décalage
        // sous-pixel fait passer d'un pixel à l'autre, garde son lissage).
        conf = smoothstep(0.0, 0.25, dot(w4, s4));
    }

    // Pixel immobile (caméra arrêtée) : l'historique est exact, seul le décalage sous-pixel
    // change d'une image à l'autre. On l'accepte alors largement (boîte min/max des voisins,
    // mélange lent) pour que la moyenne converge sans scintiller. En mouvement, boîte plus
    // serrée (écart type) et rejet plus rapide, pour éviter les traînées.
    let speed = length(motion);
    // still.y = 1 : la scène vient de changer (chunk chargé, bloc posé) : comme en mouvement.
    let moving = max(smoothstep(0.05, 1.0, speed), params.still.y);
    let hist4 = sample_history(prev_uv);
    let hist_raw = to_ycocg(hist4.rgb);
    // (v202) Alpha de l'historique : 1 là où un objet bougeait à l'image d'avant (et sur
    // un pixel autour), divisé par deux à chaque image. Un PNJ qui marche avance de plus
    // d'un pixel par image : là où il était, plus rien ne bouge, mais l'historique a
    // encore sa couleur.
    let was_moving = hist4.a;
    let flag = max(select(0.0, 1.0, near_motion), was_moving * 0.5);
    let recent = near_motion || was_moving > 0.2;
    // Immobile : la géométrie a-t-elle changé ici (bloc posé, chunk chargé) ? On compare la
    // profondeur courante à celles de l'image précédente autour du pixel. Si rien n'a
    // changé, l'historique est bon : on ne le borne que très largement (les détails plus
    // fins qu'un pixel, vus seulement par certains décalages, ne clignotent plus).
    // En croix (le pixel et ses quatre voisins) : suffit à reconnaître la même géométrie.
    // (v204) Deux lectures groupées au lieu de six pixels : les coins haut-gauche et
    // bas-droite du pixel donnent la croix (et deux diagonales, laissées de côté).
    let h0 = textureGather(prev_depth, lin_sampler, corner / isize);
    let h1 = textureGather(prev_depth, lin_sampler, (corner + vec2<f32>(1.0, 1.0)) / isize);
    let d_prev = h0.y;
    let pmin = min(min(d_prev, h0.z), min(min(h0.x, h1.z), h1.x));
    let pmax = max(max(d_prev, h0.z), max(max(h0.x, h1.z), h1.x));
    // (v202) Un objet bouge ou bougeait tout près (`recent`) alors que ce pixel-ci est
    // immobile (caméra arrêtée) : ce qu'il y avait ici à l'image d'avant a pu partir. La
    // croix des profondeurs d'avant (qui recouvre l'objet et le fond à son bord) le
    // prendrait pour la même géométrie et garderait sa couleur : une traînée derrière
    // chaque membre, des contours laissés partout où les PNJ sont passés. Ici, seule la
    // profondeur de ce pixel-ci compte ; si elle a changé, l'historique est celui d'autre
    // chose : l'image courante seule.
    let still_here = dot(mv_here, mv_here) < 1e-4;
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
    // (v268) Image d'avant d'une autre surface (voir `conf`) : l'image courante.
    let out = mix(hist, to_ycocg(cur), mix(1.0, alpha, conf));
    return vec4<f32>(from_ycocg(out), flag);
}

/// (v268) La profondeur `dp` de l'image d'avant est-elle celle de la même surface que `d` (Z
/// inversé, 0 : le ciel), à `tol` près (part de la plus proche) ? 1 ou 0.
fn same_surface(dp: f32, d: f32, tol: f32) -> f32 {
    return select(0.0, 1.0, abs(dp - d) <= tol * max(dp, d) + 1e-7);
}

