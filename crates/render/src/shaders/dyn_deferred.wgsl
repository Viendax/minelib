// Minelib — lumières dynamiques (éditeur de lumières) ajoutées après coup à l'image du monde
// opaque : éclairage différé. Le monde et les entités laissent albédo, normale et poids dans
// deux images (gbuffer.wgsl) ; ce shader plein écran ajoute la lumière de chaque pixel
// visible une seule fois (pas sur les faces cachées derrière d'autres), sans alourdir le
// shader du monde. Même calcul que l'éclairage direct (dyn_light, dynlights.wgsl) :
//   couleur += albédo × diffus × poids diffus + reflets × poids des reflets
// (reflets teintés par l'albédo sur le métal). gbuffer.wgsl et dynlights.wgsl sont ajoutés
// à la suite.
//
// Demi-résolution, exacte aux bords (v181) : la lumière est d'abord calculée pour un pixel
// sur quatre, celui des coordonnées paires (`fs_half`, image de demi-taille). Chaque pixel
// reprend ensuite la moyenne de ses voisins calculés (`fs_interp` : lui-même, ses deux
// voisins, ou ses quatre en diagonale), mais seulement si tous sont sur sa face (même
// normale, même brillance, profondeurs dans son plan) et qu'ils reçoivent presque la même
// lumière (moins de 2 % d'écart) ou (v202) une lumière qui varie en pente douce, sans
// courbure ni saut (vu avec les voisins suivants). Sinon, au bord d'un objet, d'une ombre,
// d'un faisceau net, d'un reflet, il est laissé de côté (pochoir à 0) et calculé
// exactement par `fs_deferred`.

// Début des uniformes du monde.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    fog: vec4<f32>,
    sun: vec4<f32>,
    sel_min: vec4<f32>,
    sel_max: vec4<f32>,
    light: vec4<f32>,
    cur_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    screen: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var scene_depth: texture_depth_2d;
@group(1) @binding(1) var gb_albedo: texture_2d<f32>;
@group(1) @binding(2) var gb_surf: texture_2d<f32>;
// Cases d'écran éclairées (rectangles en NDC) : la passe ne dessine que là.
@group(1) @binding(3) var<storage, read> lit_rects: array<vec4<f32>>;
// Lumière d'un pixel sur quatre (`fs_half`, `fs_water_half`), lue par `fs_interp` et
// `fs_water_interp` seulement.
@group(1) @binding(4) var half_light: texture_2d<u32>;
// (v199) Face translucide de chaque pixel (eau, verre, glace) laissée par la passe des
// translucides, lue par `fs_water*` seulement : profondeur (bits d'un f32), albédo (racine
// carrée) et brillance, normale et poids (déjà multipliés par l'opacité de la face).
@group(1) @binding(5) var water_gb: texture_2d<u32>;

@vertex
fn vs_tiles(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
    return dl_rect_corner(vi, lit_rects[ii]);
}

/// Les cases éclairées élargies de deux pixels de l'image (un de la demi-résolution) : les
/// voisins que lit `fs_interp` au bord d'une case sont calculés.
@vertex
fn vs_tiles_half(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
    let e = vec2<f32>(4.0 / max(g.screen.x, 1.0), 4.0 / max(g.screen.y, 1.0));
    let r = lit_rects[ii];
    return dl_rect_corner(vi, vec4<f32>(r.x - e.x, r.y + e.y, r.z + e.x, r.w - e.y));
}

/// Lumière des lumières dynamiques au pixel `px` (surface `s`, albédo `a`) : diffus et
/// reflets, avant l'albédo et les poids.
fn deferred_light(px: vec2<i32>, s: vec4<f32>, a: vec4<f32>) -> DynOut {
    return light_at(px, textureLoad(scene_depth, px, 0), s, a);
}

/// La même chose pour une surface de profondeur `depth` (celle de l'opaque, ou celle de la
/// face translucide).
fn light_at(px: vec2<i32>, depth: f32, s: vec4<f32>, a: vec4<f32>) -> DynOut {
    // Position du pixel : sa profondeur, à travers la même matrice (avec le décalage du
    // TAA) que celle qui l'a dessiné, comptée depuis la caméra (pas d'inverse : voir
    // `dl_view_ray`).
    let frag = vec2<f32>(px) + 0.5;
    let ndc = vec2<f32>(frag.x / g.screen.x * 2.0 - 1.0, 1.0 - frag.y / g.screen.y * 2.0);
    let p = g.cam_pos.xyz + dl_view_offset(g.view_proj, ndc, max(depth, 1e-30));
    let n = gb_oct_decode(s.rg);
    let entity = a.a > 0.995;
    let gloss = select(a.a / 0.99, 0.15, entity);
    return dyn_light(p, n, g.cam_pos.xyz, gloss, entity, frag, g.screen.xy);
}

/// (v220) Pixel de l'opaque dans l'ombre d'un joueur à une lumière dynamique : un
/// cinquantième de pixel ajouté à son mouvement (passe additive sur l'image des mouvements,
/// comme `gb_out` de world.wgsl pour le soleil et les torches) : l'anticrénelage y borne
/// l'image d'avant de près, une ombre qui bouge ne laisse pas de traînée.
@fragment
fn fs_shadow_motion(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(frag.xy);
    let depth = textureLoad(scene_depth, px, 0);
    // (Profondeur inversée : 0, le ciel.)
    if depth <= 0.0 {
        discard;
    }
    let s = textureLoad(gb_surf, px, 0);
    let a = textureLoad(gb_albedo, px, 0);
    let fc = vec2<f32>(px) + 0.5;
    let ndc = vec2<f32>(fc.x / g.screen.x * 2.0 - 1.0, 1.0 - fc.y / g.screen.y * 2.0);
    let p = g.cam_pos.xyz + dl_view_offset(g.view_proj, ndc, depth);
    let n = gb_oct_decode(s.rg);
    if !dl_player_shadowed(p, n, a.a > 0.995, fc, g.screen.xy) {
        discard;
    }
    return vec4<f32>(0.02, 0.0, 0.0, 0.0);
}

/// Ce que la lumière `d` ajoute au pixel (albédo `a`, poids diffus `w`, reflets `spec`).
fn deferred_color(d: DynOut, a: vec4<f32>, w: f32, spec: vec2<f32>) -> vec4<f32> {
    let sp = select(d.specular, d.specular * a.rgb * 1.6, spec.y > 0.5);
    return vec4<f32>(a.rgb * d.diffuse * w + sp * spec.x, 0.0);
}

/// (v215) La même chose pour un pixel de l'opaque, avec le voile noir des entités
/// (`gb_dark_veil` : albédo.a = 1).
fn opaque_color(d: DynOut, a: vec4<f32>, w: f32, spec: vec2<f32>) -> vec4<f32> {
    let c = deferred_color(d, a, w, spec);
    if a.a > 0.995 {
        return c * gb_dark_veil(a.rgb);
    }
    return c;
}

/// Clé d'une surface : normale (octaèdre, 2 × 8 bits), brillance (8 bits), bit 24 : la
/// lumière a été calculée. Deux pixels de même clé sont sur des faces parallèles d'un même
/// matériau (le plan se vérifie à part, par les profondeurs).
fn half_key(s: vec4<f32>, a: vec4<f32>) -> u32 {
    let n = vec2<u32>(round(clamp(s.rg, vec2<f32>(0.0), vec2<f32>(1.0)) * 255.0));
    let gl = u32(round(clamp(a.a, 0.0, 1.0) * 255.0));
    return n.x | (n.y << 8u) | (gl << 16u) | (1u << 24u);
}

/// Lumière d'un pixel sur quatre : celui des coordonnées paires (2x, 2y) de chaque carré de
/// 2 × 2, calculée exactement comme par `fs_deferred`, rangée en demi-flottants avec la clé
/// de sa surface (0 : rien à éclairer).
@fragment
fn fs_half(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<u32> {
    let px = vec2<i32>(frag.xy) * 2;
    let dims = vec2<i32>(textureDimensions(gb_surf));
    if px.x >= dims.x || px.y >= dims.y {
        return vec4<u32>(0u);
    }
    let s = textureLoad(gb_surf, px, 0);
    let spec = gb_unpack_spec(s.a);
    if s.b <= 0.0 && spec.x <= 0.0 {
        return vec4<u32>(0u);
    }
    let a = textureLoad(gb_albedo, px, 0);
    return half_pack(deferred_light(px, s, a), half_key(s, a));
}

/// Lumière `d` d'un pixel calculé, en demi-flottants, avec la clé de sa surface.
fn half_pack(d: DynOut, key: u32) -> vec4<u32> {
    return vec4<u32>(
        pack2x16float(d.diffuse.rg),
        pack2x16float(vec2<f32>(d.diffuse.b, d.specular.r)),
        pack2x16float(d.specular.gb),
        key,
    );
}

/// Lumière d'un voisin calculé : diffus (xyz) et reflets, dépaquetés.
struct HalfTap {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
};

fn half_tap(t: vec4<u32>) -> HalfTap {
    let a = unpack2x16float(t.x);
    let b = unpack2x16float(t.y);
    let c = unpack2x16float(t.z);
    return HalfTap(vec3<f32>(a, b.x), vec3<f32>(b.y, c));
}

/// Écart entre les plus fortes et les plus faibles de quatre lumières, comparé à la plus
/// forte : vrai si elles se tiennent à 2 % près (variation douce, que la moyenne rend).
fn half_close(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> bool {
    let hi = max(max(a, b), max(c, d));
    let lo = min(min(a, b), min(c, d));
    return all(hi - lo <= hi * 0.02 + vec3<f32>(1e-4));
}

/// Les quatre voisins calculés d'un pixel `px` (coordonnées dans l'image de demi-taille) et
/// leur lumière : le pixel lui-même (coordonnées paires), ses deux voisins (une impaire) ou
/// ses quatre en diagonale (deux impaires) ; un voisin compté deux fois ne change pas la
/// moyenne.
struct HalfQuad {
    q00: vec2<i32>,
    q10: vec2<i32>,
    q01: vec2<i32>,
    q11: vec2<i32>,
    t00: vec4<u32>,
    t10: vec4<u32>,
    t01: vec4<u32>,
    t11: vec4<u32>,
};

fn half_quad(px: vec2<i32>) -> HalfQuad {
    let h = px / 2;
    let o = vec2<i32>(px.x & 1, px.y & 1);
    let hi = vec2<i32>(textureDimensions(half_light)) - 1;
    let q00 = min(h, hi);
    let q10 = min(h + vec2<i32>(o.x, 0), hi);
    let q01 = min(h + vec2<i32>(0, o.y), hi);
    let q11 = min(h + o, hi);
    return HalfQuad(
        q00, q10, q01, q11,
        textureLoad(half_light, q00, 0),
        textureLoad(half_light, q10, 0),
        textureLoad(half_light, q01, 0),
        textureLoad(half_light, q11, 0),
    );
}

/// Tous les voisins sont sur la face de clé `key` (même normale, même matériau, lumière
/// calculée).
fn half_same(q: HalfQuad, key: u32) -> bool {
    let m = 0x1ffffffu;
    return (q.t00.w & m) == key && (q.t10.w & m) == key && (q.t01.w & m) == key
        && (q.t11.w & m) == key;
}

/// Dans son plan : sur une face plane, la profondeur (Z inversé ∝ 1 / distance) varie
/// linéairement à l'écran, et le pixel (profondeur `d`) est au milieu de ses voisins
/// calculés (somme de leurs profondeurs : `dsum`).
fn half_in_plane(d: f32, dsum: f32) -> bool {
    return abs(dsum * 0.25 - d) <= d * 1e-3;
}

/// (v202) Lumière d'un voisin calculé plus loin (`c`, coordonnées de demi-taille), s'il
/// est sur la face de clé `key` ; sinon diffus < 0.
fn half_far(c: vec2<i32>, key: u32) -> HalfTap {
    let hi = vec2<i32>(textureDimensions(half_light)) - 1;
    let t = textureLoad(half_light, clamp(c, vec2<i32>(0), hi), 0);
    if (t.w & 0x1ffffffu) != key {
        return HalfTap(vec3<f32>(-1.0), vec3<f32>(0.0));
    }
    return half_tap(t);
}

/// (v202) Sur une variation douce (pente d'un bord de faisceau étalé, d'une atténuation,
/// d'un grand reflet), la moyenne de deux voisins calculés `b` et `c` vaut le milieu même
/// s'ils diffèrent de plus de 2 %. Les voisins suivants, `a` avant et `d` après, le disent :
/// ni courbure (écart entre le milieu de la courbe qui passe par les quatre et celui de la
/// droite : (d − c − b + a) / 16), ni saut (pente du milieu loin de la moyenne de celles
/// d'à côté : un bord d'ombre net entre `b` et `c`) au-delà de 0,4 % de la plus forte.
fn half_line(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> bool {
    let tol = max(max(a, b), max(c, d)) * 0.004 + vec3<f32>(1e-4);
    let sm = b - a;
    let sc = c - b;
    let sp = d - c;
    return all(abs(sp - sm) * (1.0 / 16.0) <= tol) && all(abs(sc - (sm + sp) * 0.5) <= tol * 2.0);
}

/// (v202) `half_line` pour le diffus et les reflets.
fn half_line_tap(a: HalfTap, b: HalfTap, c: HalfTap, d: HalfTap) -> bool {
    return a.diffuse.x >= 0.0 && d.diffuse.x >= 0.0
        && half_line(a.diffuse, b.diffuse, c.diffuse, d.diffuse)
        && half_line(a.specular, b.specular, c.specular, d.specular);
}

/// Moyenne de la lumière des voisins du pixel `px` (face de clé `key`), si elle vaut le
/// calcul exact : presque la même lumière partout (2 %), ou (v202) une variation douce,
/// sans courbure ni saut (`half_line`, avec les voisins suivants sur la même face). Pas
/// au bord d'une ombre, d'un faisceau net, d'un gobo, ni au sommet d'une tache de reflet.
/// Sinon diffus < 0.
fn half_average(q: HalfQuad, px: vec2<i32>, key: u32) -> DynOut {
    let l00 = half_tap(q.t00);
    let l10 = half_tap(q.t10);
    let l01 = half_tap(q.t01);
    let l11 = half_tap(q.t11);
    let avg = DynOut(
        (l00.diffuse + l10.diffuse + l01.diffuse + l11.diffuse) * 0.25,
        (l00.specular + l10.specular + l01.specular + l11.specular) * 0.25,
    );
    if half_close(l00.diffuse, l10.diffuse, l01.diffuse, l11.diffuse)
        && half_close(l00.specular, l10.specular, l01.specular, l11.specular) {
        return avg;
    }
    let h = px / 2;
    let o = vec2<i32>(px.x & 1, px.y & 1);
    var ok = true;
    // Entre deux voisins de la même ligne : ceux d'avant et d'après sur cette ligne.
    if o.x == 1 {
        ok = ok && half_line_tap(
            half_far(h - vec2<i32>(1, 0), key), l00, l10, half_far(h + vec2<i32>(2, 0), key),
        );
    }
    // De même en colonne.
    if ok && o.y == 1 {
        ok = ok && half_line_tap(
            half_far(h - vec2<i32>(0, 1), key), l00, l01, half_far(h + vec2<i32>(0, 2), key),
        );
    }
    // Au milieu de quatre : le quatrième dans le plan des trois autres (pas d'ombre ni de
    // bord qui ne toucherait que lui).
    if ok && o.x == 1 && o.y == 1 {
        let tol = max(max(l00.diffuse, l10.diffuse), max(l01.diffuse, l11.diffuse)) * 0.004
            + vec3<f32>(1e-4);
        let tol_s = max(max(l00.specular, l10.specular), max(l01.specular, l11.specular)) * 0.004
            + vec3<f32>(1e-4);
        ok = all(abs(l11.diffuse - l10.diffuse - l01.diffuse + l00.diffuse) <= tol * 2.0)
            && all(abs(l11.specular - l10.specular - l01.specular + l00.specular) <= tol_s * 2.0);
    }
    if !ok {
        return DynOut(vec3<f32>(-1.0), vec3<f32>(0.0));
    }
    return avg;
}

/// Moyenne des voisins calculés si elle vaut le calcul exact (voir en tête) ; sinon le
/// pixel est laissé à `fs_deferred` (rien d'écrit, le pochoir reste à 0).
@fragment
fn fs_interp(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(frag.xy);
    let s = textureLoad(gb_surf, px, 0);
    let spec = gb_unpack_spec(s.a);
    if s.b <= 0.0 && spec.x <= 0.0 {
        return vec4<f32>(0.0);
    }
    // Demi-résolution décochée : tout est calculé exactement.
    if (dh.vl_c.w & DL_HALF_LIGHT) == 0u {
        discard;
    }
    let a = textureLoad(gb_albedo, px, 0);
    let q = half_quad(px);
    let key = half_key(s, a);
    if !half_same(q, key) {
        discard;
    }
    let full = vec2<i32>(textureDimensions(scene_depth)) - 1;
    let dsum = textureLoad(scene_depth, min(q.q00 * 2, full), 0)
        + textureLoad(scene_depth, min(q.q10 * 2, full), 0)
        + textureLoad(scene_depth, min(q.q01 * 2, full), 0)
        + textureLoad(scene_depth, min(q.q11 * 2, full), 0);
    if !half_in_plane(textureLoad(scene_depth, px, 0), dsum) {
        discard;
    }
    let avg = half_average(q, px, key);
    if avg.diffuse.x < 0.0 {
        discard;
    }
    return opaque_color(avg, a, s.b, spec);
}

/// Calcul exact : les pixels que `fs_interp` a laissés (pochoir à 0), ou tous quand la
/// demi-résolution est décochée.
@fragment
fn fs_deferred(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // (Seulement dans les cases éclairées : `vs_tiles` ne dessine que leurs rectangles.)
    let px = vec2<i32>(frag.xy);
    let s = textureLoad(gb_surf, px, 0);
    let spec = gb_unpack_spec(s.a);
    if s.b <= 0.0 && spec.x <= 0.0 {
        return vec4<f32>(0.0);
    }
    let a = textureLoad(gb_albedo, px, 0);
    return opaque_color(deferred_light(px, s, a), a, s.b, spec);
}

/// (v199) Lumières sur l'eau, le verre et la glace : la face translucide de chaque pixel
/// (dernière dessinée), éclairée comme l'opaque — un pixel sur quatre, la moyenne des
/// voisins là où elle vaut le calcul exact, le calcul exact ailleurs — et ajoutée
/// par-dessus. Le mélange de la face est déjà dans ses poids : même résultat que
/// l'éclairage direct dans le shader du monde, sans l'alourdir (et une seule fois par
/// pixel).
struct WaterFace {
    /// Profondeur (0 : pas de face translucide à ce pixel).
    depth: f32,
    /// Comme `gb_surf` : normale, poids diffus, reflets.
    s: vec4<f32>,
    /// Comme `gb_albedo` : albédo, brillance.
    a: vec4<f32>,
};

fn water_face(px: vec2<i32>) -> WaterFace {
    let w = textureLoad(water_gb, px, 0);
    let al = unpack4x8unorm(w.y);
    return WaterFace(bitcast<f32>(w.x), unpack4x8unorm(w.z), vec4<f32>(al.rgb * al.rgb, al.a));
}

/// Rien à ajouter à cette face (pas de face, ou ni diffus ni reflets).
fn water_unlit(f: WaterFace) -> bool {
    return f.depth <= 0.0 || (f.s.b <= 0.0 && gb_unpack_spec(f.s.a).x <= 0.0);
}

/// Comme `fs_half`, sur la face translucide.
@fragment
fn fs_water_half(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<u32> {
    let px = vec2<i32>(frag.xy) * 2;
    let dims = vec2<i32>(textureDimensions(water_gb));
    if px.x >= dims.x || px.y >= dims.y {
        return vec4<u32>(0u);
    }
    let f = water_face(px);
    if water_unlit(f) {
        return vec4<u32>(0u);
    }
    return half_pack(light_at(px, f.depth, f.s, f.a), half_key(f.s, f.a));
}

/// Comme `fs_interp`, sur la face translucide (sa profondeur à elle).
@fragment
fn fs_water_interp(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(frag.xy);
    let f = water_face(px);
    if water_unlit(f) {
        return vec4<f32>(0.0);
    }
    if (dh.vl_c.w & DL_HALF_LIGHT) == 0u {
        discard;
    }
    let q = half_quad(px);
    let key = half_key(f.s, f.a);
    if !half_same(q, key) {
        discard;
    }
    let full = vec2<i32>(textureDimensions(water_gb)) - 1;
    let dsum = bitcast<f32>(textureLoad(water_gb, min(q.q00 * 2, full), 0).x)
        + bitcast<f32>(textureLoad(water_gb, min(q.q10 * 2, full), 0).x)
        + bitcast<f32>(textureLoad(water_gb, min(q.q01 * 2, full), 0).x)
        + bitcast<f32>(textureLoad(water_gb, min(q.q11 * 2, full), 0).x);
    if !half_in_plane(f.depth, dsum) {
        discard;
    }
    let avg = half_average(q, px, key);
    if avg.diffuse.x < 0.0 {
        discard;
    }
    return deferred_color(avg, f.a, f.s.b, gb_unpack_spec(f.s.a));
}

/// Comme `fs_deferred`, sur la face translucide : les pixels que `fs_water_interp` a
/// laissés (pochoir à 0), ou tous.
@fragment
fn fs_water(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(frag.xy);
    let f = water_face(px);
    if water_unlit(f) {
        return vec4<f32>(0.0);
    }
    return deferred_color(light_at(px, f.depth, f.s, f.a), f.a, f.s.b, gb_unpack_spec(f.s.a));
}
