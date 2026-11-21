// Minelib — lumière volumétrique (demi- ou quart de résolution) ajoutée à l'image du monde
// en pleine résolution. Chaque pixel de l'image volumétrique a deux couches (jusqu'à la surface
// la plus proche et jusqu'à la plus lointaine des pixels qu'il couvre, avec leurs profondeurs).
// Chaque pixel du monde prend, chez les quatre voisins, la couche qui est à sa propre distance,
// pondérée par la place du voisin et par l'accord des profondeurs : les rayons ne bavent pas
// d'un objet proche sur le fond (ni l'inverse), et passent par les trous d'un feuillage.

@group(0) @binding(0) var vl_tex: texture_2d<u32>;
@group(0) @binding(1) var full_depth: texture_depth_2d;
// Cases d'écran éclairées (rectangles en NDC) : seulement là qu'il peut y avoir des rayons
// (ailleurs, la recomposition n'ajoutait que des zéros). La marche a rempli ces cases,
// élargies de quelques pixels : les voisins lus ici en font partie.
@group(0) @binding(2) var<storage, read> lit_rects: array<vec4<f32>>;

@vertex
fn vs_tiles(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
    // Coins (0,0) (1,0) (0,1) (0,1) (1,0) (1,1) : bits de x = 110010, de y = 101100.
    let r = lit_rects[ii];
    let cx = f32((50u >> vi) & 1u);
    let cy = f32((44u >> vi) & 1u);
    return vec4<f32>(mix(r.x, r.z, cx), mix(r.y, r.w, cy), 0.0, 1.0);
}

/// Un voisin : lumière jusqu'à sa surface la plus proche (`n`, profondeur `dn`) et jusqu'à la
/// plus lointaine (`f`, profondeur `df`). Profondeurs en Z inversé (∝ 1 / distance, 0 : ciel).
struct VlTap {
    n: vec3<f32>,
    dn: f32,
    f: vec3<f32>,
    df: f32,
};

fn vl_tap(i: vec2<i32>) -> VlTap {
    let v = textureLoad(vl_tex, i, 0);
    let a = unpack2x16float(v.x);
    let b = unpack2x16float(v.y);
    let c = unpack2x16float(v.z);
    let e = unpack2x16float(v.w);
    return VlTap(vec3<f32>(a, b.x), b.y, vec3<f32>(c, e.x), e.y);
}

/// Lumière d'un voisin pour un pixel de profondeur `d`, pondérée (rgb × poids, poids) : sa
/// couche proche si le pixel est devant sa surface la plus proche (ou dessus), la lointaine
/// s'il est derrière la plus lointaine (ou dessus), entre les deux selon la distance. Poids :
/// place (bilinéaire `b`) et écart relatif de la distance hors de l'intervalle du voisin.
fn vl_pick(s: VlTap, d: f32, b: f32) -> vec4<f32> {
    var e: vec3<f32>;
    var dz = 0.0;
    if d >= s.dn {
        e = s.n;
        dz = (d - s.dn) / max(d, 1e-6);
    } else if d <= s.df {
        e = s.f;
        dz = (s.df - d) / max(s.df, 1e-6);
    } else {
        // Entre les deux surfaces : part de la distance (1 / profondeur) parcourue de la
        // proche à la lointaine.
        let k = (s.dn - d) * s.df / max(d * (s.dn - s.df), 1e-12);
        e = mix(s.n, s.f, clamp(k, 0.0, 1.0));
    }
    let w = b * exp(-dz * 24.0) + 1e-5 * b;
    return vec4<f32>(e * w, w);
}

/// (v215) Tout l'écran de l'image volumétrique, en un triangle.
@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

/// (v215) Lissage des rayons, entre la marche et la recomposition : chaque pixel de l'image
/// volumétrique (demi- ou quart de résolution) prend la moyenne pondérée de ses 3 × 3
/// voisins (tente 1-2-1), la couche proche avec les couches proches à sa distance, la
/// lointaine avec les lointaines (mêmes poids de profondeur que `vl_pick` : rien ne bave
/// d'un objet sur le fond). La marche décale ses pas d'un tramage différent à chaque pixel
/// (et à chaque image) : seule, l'image grésille et l'anticrénelage la lisse en quelques
/// images, ce qui se voit en tournant vite. Moyennée sur ses voisins, elle est lisse tout de
/// suite. Neuf lectures par pixel de l'image réduite : presque rien.
@fragment
fn fs_smooth(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<u32> {
    let hd = vec2<i32>(textureDimensions(vl_tex));
    let c = vec2<i32>(floor(frag.xy));
    let s = vl_tap(c);
    var near = vec4<f32>(0.0);
    var far = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let q = vl_tap(clamp(c + vec2<i32>(x, y), vec2<i32>(0), hd - 1));
            let b = f32((2 - abs(x)) * (2 - abs(y)));
            let wn = b * exp(-abs(q.dn - s.dn) / max(max(q.dn, s.dn), 1e-6) * 24.0);
            let wf = b * exp(-abs(q.df - s.df) / max(max(q.df, s.df), 1e-6) * 24.0);
            near += vec4<f32>(q.n * wn, wn);
            far += vec4<f32>(q.f * wf, wf);
        }
    }
    // Le pixel lui-même a toujours son poids (4) : jamais de division par zéro.
    let n = near.rgb / near.w;
    let f = far.rgb / far.w;
    return vec4<u32>(
        pack2x16float(n.rg),
        pack2x16float(vec2<f32>(n.b, s.dn)),
        pack2x16float(f.rg),
        pack2x16float(vec2<f32>(f.b, s.df)),
    );
}

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fd = vec2<i32>(textureDimensions(full_depth));
    let hd = vec2<i32>(textureDimensions(vl_tex));
    // Demi- ou quart de résolution : rapport des tailles des deux images.
    let ratio = vec2<f32>(fd) / vec2<f32>(hd);
    let f = frag.xy / ratio - 0.5;
    let i0 = vec2<i32>(floor(f));
    let fw = f - floor(f);
    let hi = hd - 1;
    let s00 = vl_tap(clamp(i0, vec2<i32>(0), hi));
    let s10 = vl_tap(clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), hi));
    let s01 = vl_tap(clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), hi));
    let s11 = vl_tap(clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), hi));
    // Aucun rayon chez les quatre voisins (hors des faisceaux) : rien à ajouter, quels que
    // soient les poids. Ni profondeur à lire, ni exponentielles.
    let m = max(max(max(s00.n, s10.n), max(s01.n, s11.n)), max(max(s00.f, s10.f), max(s01.f, s11.f)));
    if max(m.x, max(m.y, m.z)) <= 0.0 {
        return vec4<f32>(0.0);
    }
    let d = textureLoad(full_depth, clamp(vec2<i32>(floor(frag.xy)), vec2<i32>(0), fd - 1), 0);
    let acc = vl_pick(s00, d, (1.0 - fw.x) * (1.0 - fw.y))
        + vl_pick(s10, d, fw.x * (1.0 - fw.y))
        + vl_pick(s01, d, (1.0 - fw.x) * fw.y)
        + vl_pick(s11, d, fw.x * fw.y);
    return vec4<f32>(acc.rgb / max(acc.w, 1e-6), 0.0);
}
