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
// lumière (moins de 2 % d'écart). Sinon, au bord d'un objet, d'une ombre, d'un faisceau,
// d'un reflet, il est laissé de côté (pochoir à 0) et calculé exactement par `fs_deferred`.

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
// Lumière d'un pixel sur quatre (`fs_half`), lue par `fs_interp` seulement.
@group(1) @binding(4) var half_light: texture_2d<u32>;

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
    // Position du pixel : sa profondeur, à travers la même matrice (avec le décalage du
    // TAA) que celle qui l'a dessiné, comptée depuis la caméra (pas d'inverse : voir
    // `dl_view_ray`).
    let frag = vec2<f32>(px) + 0.5;
    let depth = textureLoad(scene_depth, px, 0);
    let ndc = vec2<f32>(frag.x / g.screen.x * 2.0 - 1.0, 1.0 - frag.y / g.screen.y * 2.0);
    let p = g.cam_pos.xyz + dl_view_offset(g.view_proj, ndc, max(depth, 1e-30));
    let n = gb_oct_decode(s.rg);
    let entity = a.a > 0.995;
    let gloss = select(a.a / 0.99, 0.15, entity);
    return dyn_light(p, n, g.cam_pos.xyz, gloss, entity, frag, g.screen.xy);
}

/// Ce que la lumière `d` ajoute au pixel (albédo `a`, poids diffus `w`, reflets `spec`).
fn deferred_color(d: DynOut, a: vec4<f32>, w: f32, spec: vec2<f32>) -> vec4<f32> {
    let sp = select(d.specular, d.specular * a.rgb * 1.6, spec.y > 0.5);
    return vec4<f32>(a.rgb * d.diffuse * w + sp * spec.x, 0.0);
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
    let d = deferred_light(px, s, a);
    return vec4<u32>(
        pack2x16float(d.diffuse.rg),
        pack2x16float(vec2<f32>(d.diffuse.b, d.specular.r)),
        pack2x16float(d.specular.gb),
        half_key(s, a),
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
    // Voisins calculés : le pixel lui-même (coordonnées paires), ses deux voisins (une
    // impaire) ou ses quatre en diagonale (deux impaires) ; un voisin compté deux fois
    // ne change pas la moyenne.
    let h = px / 2;
    let o = vec2<i32>(px.x & 1, px.y & 1);
    let hi = vec2<i32>(textureDimensions(half_light)) - 1;
    let full = vec2<i32>(textureDimensions(scene_depth)) - 1;
    let q00 = min(h, hi);
    let q10 = min(h + vec2<i32>(o.x, 0), hi);
    let q01 = min(h + vec2<i32>(0, o.y), hi);
    let q11 = min(h + o, hi);
    let t00 = textureLoad(half_light, q00, 0);
    let t10 = textureLoad(half_light, q10, 0);
    let t01 = textureLoad(half_light, q01, 0);
    let t11 = textureLoad(half_light, q11, 0);
    // Même face, même matériau, lumière calculée.
    let key = half_key(s, a);
    let m = 0x1ffffffu;
    if (t00.w & m) != key || (t10.w & m) != key || (t01.w & m) != key || (t11.w & m) != key {
        discard;
    }
    // Dans son plan : sur une face plane, la profondeur (Z inversé ∝ 1 / distance) varie
    // linéairement à l'écran, et le pixel est au milieu de ses voisins calculés.
    let d = textureLoad(scene_depth, px, 0);
    let dsum = textureLoad(scene_depth, min(q00 * 2, full), 0)
        + textureLoad(scene_depth, min(q10 * 2, full), 0)
        + textureLoad(scene_depth, min(q01 * 2, full), 0)
        + textureLoad(scene_depth, min(q11 * 2, full), 0);
    if abs(dsum * 0.25 - d) > d * 1e-3 {
        discard;
    }
    // Presque la même lumière partout : ni bord d'ombre, de faisceau ou de gobo, ni tache
    // de reflet entre eux.
    let l00 = half_tap(t00);
    let l10 = half_tap(t10);
    let l01 = half_tap(t01);
    let l11 = half_tap(t11);
    if !half_close(l00.diffuse, l10.diffuse, l01.diffuse, l11.diffuse)
        || !half_close(l00.specular, l10.specular, l01.specular, l11.specular) {
        discard;
    }
    let avg = DynOut(
        (l00.diffuse + l10.diffuse + l01.diffuse + l11.diffuse) * 0.25,
        (l00.specular + l10.specular + l01.specular + l11.specular) * 0.25,
    );
    return deferred_color(avg, a, s.b, spec);
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
    return deferred_color(deferred_light(px, s, a), a, s.b, spec);
}
