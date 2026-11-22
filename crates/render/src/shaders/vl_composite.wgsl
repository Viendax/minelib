// Minelib — lumière volumétrique (demi- ou quart de résolution) ajoutée à l'image du monde
// en pleine résolution. Chaque pixel de l'image volumétrique a deux couches (jusqu'à la surface
// la plus proche et jusqu'à la plus lointaine des pixels qu'il couvre, avec leurs profondeurs).
// Chaque pixel du monde prend, chez les quatre voisins, la couche qui est à sa propre distance,
// pondérée par la place du voisin et par l'accord des profondeurs : les rayons ne bavent pas
// d'un objet proche sur le fond (ni l'inverse), et passent par les trous d'un feuillage.
// (v223) Entre la marche et la recomposition, `fs_temporal` reprend l'image des lumières
// posées d'une image à l'autre (elle reste lisse quand on tourne), et lisse celle des
// lumières portées.

// Recomposition : l'image finale des rayons ; reprise (`fs_temporal`) : la marche des
// lumières posées.
@group(0) @binding(0) var vl_tex: texture_2d<u32>;
@group(0) @binding(1) var full_depth: texture_depth_2d;
// Cases d'écran éclairées (rectangles en NDC) : seulement là qu'il peut y avoir des rayons
// (ailleurs, la recomposition n'ajoutait que des zéros). La marche a rempli ces cases,
// élargies de quelques pixels : les voisins lus ici en font partie.
@group(0) @binding(2) var<storage, read> lit_rects: array<vec4<f32>>;
// (v223) Reprise : la marche des lumières portées, et l'image des lumières posées de
// l'image précédente (historique).
@group(0) @binding(3) var vl_carried: texture_2d<u32>;
@group(0) @binding(4) var vl_hist: texture_2d<u32>;

/// (v223) Même structure que dans volumetric.wgsl.
struct VlFrame {
    cur_rel: mat4x4<f32>,
    prev_rel: mat4x4<f32>,
    delta: vec4<f32>,
    margin: vec4<f32>,
    size: vec4<f32>,
    jitter: vec4<f32>,
};
@group(0) @binding(5) var<uniform> vf: VlFrame;

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
    return vl_unpack(textureLoad(vl_tex, i, 0));
}

fn vl_unpack(v: vec4<u32>) -> VlTap {
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

fn vl_pack(n: vec3<f32>, dn: f32, f: vec3<f32>, df: f32) -> vec4<u32> {
    return vec4<u32>(
        pack2x16float(n.rg),
        pack2x16float(vec2<f32>(n.b, dn)),
        pack2x16float(f.rg),
        pack2x16float(vec2<f32>(f.b, df)),
    );
}

/// (v223) Pixel des rayons (marge comprise) sur l'écran ?
fn vl_on_screen(c: vec2<i32>) -> bool {
    let k = max(vf.margin.z, 1.0);
    let cell = c - vec2<i32>(vf.margin.xy);
    let on_w = vec2<i32>(ceil(vf.size.xy / k));
    return all(cell >= vec2<i32>(0)) && all(cell < on_w);
}

/// (v223) Direction (non normée, profondeur de vue 1) du point `ndc` de l'écran, tirée des
/// lignes de la matrice comptée depuis la caméra (comme `dl_view_ray`).
fn vl_ray(m: mat4x4<f32>, ndc: vec2<f32>) -> vec3<f32> {
    let rx = vec3<f32>(m[0][0], m[1][0], m[2][0]);
    let ry = vec3<f32>(m[0][1], m[1][1], m[2][1]);
    let rw = vec3<f32>(m[0][3], m[1][3], m[2][3]);
    let d = cross(rx - ndc.x * rw, ry - ndc.y * rw);
    return d / dot(rw, d);
}

/// (v223) Lumière d'une couche à la profondeur `z` (Z inversé, 0 : ciel) le long de `ray`,
/// dans l'image précédente : (rgb × poids, poids). Le point est ramené à la caméra d'avant
/// (exact quand on ne fait que tourner), puis lu chez ses quatre voisins de l'historique,
/// pondérés par la place et l'accord de leur profondeur avec celle qu'il avait alors.
fn vl_history(ray: vec3<f32>, z: f32, far: bool) -> vec4<f32> {
    let near_plane = vf.cur_rel[3][2];
    var pc: vec4<f32>;
    if z > 0.0 {
        pc = vf.prev_rel * vec4<f32>(ray * (near_plane / z) + vf.delta.xyz, 1.0);
    } else {
        // Ciel (ou rien) : seulement la direction.
        pc = vf.prev_rel * vec4<f32>(ray, 0.0);
    }
    if pc.w <= 1e-6 {
        return vec4<f32>(0.0);
    }
    let ndc = pc.xy / pc.w;
    let expect = max(pc.z / pc.w, 0.0);
    let k = max(vf.margin.z, 1.0);
    let p = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * vf.size.xy / k + vf.margin.xy - 0.5;
    let i0 = vec2<i32>(floor(p));
    let fw = p - floor(p);
    let hd = vec2<i32>(textureDimensions(vl_hist));
    var acc = vec4<f32>(0.0);
    for (var j = 0; j < 4; j++) {
        let o = vec2<i32>(j & 1, j >> 1);
        let q = i0 + o;
        if any(q < vec2<i32>(0)) || any(q >= hd) {
            continue;
        }
        let b = select(1.0 - fw.x, fw.x, o.x == 1) * select(1.0 - fw.y, fw.y, o.y == 1);
        let h = vl_unpack(textureLoad(vl_hist, q, 0));
        let hz = select(h.dn, h.df, far);
        let dz = abs(hz - expect) / max(max(hz, expect), 1e-6);
        let w = b * exp(-dz * 24.0);
        acc += vec4<f32>(select(h.n, h.f, far) * w, w);
    }
    return acc;
}

/// (v223) Marge pas marchée à cette image : l'historique gardé tel quel, ramené à cette
/// caméra par la seule direction (exact en tournant), lu chez ses quatre voisins (place
/// seulement), profondeurs du plus proche. `w` : part lue dans l'historique.
fn vl_keep(ray: vec3<f32>) -> VlTap {
    var out = VlTap(vec3<f32>(0.0), 0.0, vec3<f32>(0.0), 0.0);
    let pc = vf.prev_rel * vec4<f32>(ray, 0.0);
    if pc.w <= 1e-6 {
        return out;
    }
    let ndc = pc.xy / pc.w;
    let k = max(vf.margin.z, 1.0);
    let p = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * vf.size.xy / k + vf.margin.xy - 0.5;
    let i0 = vec2<i32>(floor(p));
    let fw = p - floor(p);
    let hd = vec2<i32>(textureDimensions(vl_hist));
    var wsum = 0.0;
    var best = -1.0;
    for (var j = 0; j < 4; j++) {
        let o = vec2<i32>(j & 1, j >> 1);
        let q = i0 + o;
        if any(q < vec2<i32>(0)) || any(q >= hd) {
            continue;
        }
        let b = select(1.0 - fw.x, fw.x, o.x == 1) * select(1.0 - fw.y, fw.y, o.y == 1);
        let h = vl_unpack(textureLoad(vl_hist, q, 0));
        out.n += h.n * b;
        out.f += h.f * b;
        wsum += b;
        if b > best {
            best = b;
            out.dn = h.dn;
            out.df = h.df;
        }
    }
    out.n /= max(wsum, 1e-6);
    out.f /= max(wsum, 1e-6);
    return out;
}

/// (v223) Sortie de la reprise : l'historique de l'image suivante (lumières posées) et
/// l'image que lit la recomposition (posées + portées).
struct TemporalOut {
    @location(0) hist: vec4<u32>,
    @location(1) combined: vec4<u32>,
};

/// (v223) Rayons repris d'une image à l'autre (« quand on bouge la souris, ça pixelise
/// encore un peu ») :
/// - lumières posées : l'image précédente, ramenée à la caméra d'aujourd'hui (exact en
///   tournant), mêlée à la nouvelle (`vf.jitter.z` de nouveau à chaque image), bornée par les
///   voisins de la nouvelle (une lumière qui change ne laisse pas de traînée). L'échantillon
///   de la marche bouge dans son carré à chaque image : repris, il donne l'image de tout le
///   carré. Là où l'image d'avant ne dit rien (premier coup d'œil, objet qui découvre le
///   fond), le lissage de la v215 (moyenne 3 × 3 pondérée par les profondeurs) ;
/// - lumières portées (elles bougent avec la caméra) : lissées seulement, comme avant ;
/// - la marge hors de l'écran, marchée une image sur deux : gardée telle quelle entre deux.
@fragment
fn fs_temporal(@builtin(position) frag: vec4<f32>) -> TemporalOut {
    let hd = vec2<i32>(textureDimensions(vl_tex));
    let c = vec2<i32>(floor(frag.xy));
    let margin_now = vf.margin.w > 0.5;
    let fresh = margin_now || vl_on_screen(c);
    // Direction du centre du pixel (sans le décalage de la marche).
    let k = max(vf.margin.z, 1.0);
    let mid = (vec2<f32>(c) - vf.margin.xy + 0.5) * k;
    let uv = mid / max(vf.size.xy, vec2<f32>(1.0));
    let ray = vl_ray(vf.cur_rel, vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0));
    var o: TemporalOut;
    if !fresh {
        // Marge pas marchée à cette image : gardée telle quelle (rien sans historique).
        var kept = VlTap(vec3<f32>(0.0), 0.0, vec3<f32>(0.0), 0.0);
        if vf.delta.w > 0.5 {
            kept = vl_keep(ray);
        }
        o.hist = vl_pack(kept.n, kept.dn, kept.f, kept.df);
        o.combined = o.hist;
        return o;
    }
    let s = vl_tap(c);
    var near = vec4<f32>(0.0);
    var far = vec4<f32>(0.0);
    var near_d = vec4<f32>(0.0);
    var far_d = vec4<f32>(0.0);
    var n_lo = vec3<f32>(1e9);
    var n_hi = vec3<f32>(0.0);
    var f_lo = vec3<f32>(1e9);
    var f_hi = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let qi = clamp(c + vec2<i32>(x, y), vec2<i32>(0), hd - 1);
            // Pixel de la marge pas marché à cette image : rien à lire.
            if !margin_now && !vl_on_screen(qi) {
                continue;
            }
            let q = vl_tap(qi);
            let qd = vl_unpack(textureLoad(vl_carried, qi, 0));
            let b = f32((2 - abs(x)) * (2 - abs(y)));
            let an = exp(-abs(q.dn - s.dn) / max(max(q.dn, s.dn), 1e-6) * 24.0);
            let af = exp(-abs(q.df - s.df) / max(max(q.df, s.df), 1e-6) * 24.0);
            near += vec4<f32>(q.n * b * an, b * an);
            far += vec4<f32>(q.f * b * af, b * af);
            near_d += vec4<f32>(qd.n * b * an, b * an);
            far_d += vec4<f32>(qd.f * b * af, b * af);
            if an > 0.5 {
                n_lo = min(n_lo, q.n);
                n_hi = max(n_hi, q.n);
            }
            if af > 0.5 {
                f_lo = min(f_lo, q.f);
                f_hi = max(f_hi, q.f);
            }
        }
    }
    let sn = near.rgb / max(near.w, 1e-6);
    let sf = far.rgb / max(far.w, 1e-6);
    let sn_d = near_d.rgb / max(near_d.w, 1e-6);
    let sf_d = far_d.rgb / max(far_d.w, 1e-6);
    // Image précédente, ramenée à cette caméra.
    var hn = vec4<f32>(0.0);
    var hf = vec4<f32>(0.0);
    if vf.delta.w > 0.5 {
        hn = vl_history(ray, s.dn, false);
        hf = vl_history(ray, s.df, true);
    }
    let a = clamp(vf.jitter.z, 0.02, 1.0);
    let kn = clamp(hn.w * 1.25 - 0.25, 0.0, 1.0);
    let kf = clamp(hf.w * 1.25 - 0.25, 0.0, 1.0);
    let hist_n = hn.rgb / max(hn.w, 1e-6);
    let hist_f = hf.rgb / max(hf.w, 1e-6);
    // Bornée par les voisins d'aujourd'hui (un peu élargis) : pas de traînée.
    let pad_n = (n_hi - n_lo) * 0.25 + vec3<f32>(1e-4);
    let pad_f = (f_hi - f_lo) * 0.25 + vec3<f32>(1e-4);
    let cn = clamp(hist_n, n_lo - pad_n, n_hi + pad_n);
    let cf = clamp(hist_f, f_lo - pad_f, f_hi + pad_f);
    // Historique bon : la nouvelle valeur telle quelle (son échantillon bouge à chaque
    // image) ; sinon lissée.
    let out_n = mix(sn, mix(s.n, cn, 1.0 - a), kn);
    let out_f = mix(sf, mix(s.f, cf, 1.0 - a), kf);
    o.hist = vl_pack(out_n, s.dn, out_f, s.df);
    o.combined = vl_pack(out_n + sn_d, s.dn, out_f + sf_d, s.df);
    return o;
}

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fd = vec2<i32>(textureDimensions(full_depth));
    let hd = vec2<i32>(textureDimensions(vl_tex));
    // Demi- ou quart de résolution (pixels du monde par pixel des rayons) ; (v223) l'image
    // des rayons déborde de l'écran de sa marge.
    let f = frag.xy / max(vf.margin.z, 1.0) - 0.5 + vf.margin.xy;
    let i0 = vec2<i32>(floor(f));
    let fw = f - floor(f);
    // Seulement la partie de l'écran (au bord, le pixel du bord, comme avant la marge).
    let lo = vec2<i32>(vf.margin.xy);
    let hi = min(lo + vec2<i32>(ceil(vf.size.xy / max(vf.margin.z, 1.0))), hd) - 1;
    let s00 = vl_tap(clamp(i0, lo, hi));
    let s10 = vl_tap(clamp(i0 + vec2<i32>(1, 0), lo, hi));
    let s01 = vl_tap(clamp(i0 + vec2<i32>(0, 1), lo, hi));
    let s11 = vl_tap(clamp(i0 + vec2<i32>(1, 1), lo, hi));
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
