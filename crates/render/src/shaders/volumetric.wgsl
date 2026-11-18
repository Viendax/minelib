// Minelib — lumière volumétrique des lumières dynamiques (rayons dans la brume, la fumée, la
// poussière). Calculée en demi- ou quart de résolution : pour chaque pixel, marche le long du
// regard dans le volume de chaque lumière de sa tuile (sphère ou cône), diffusion de
// Henyey-Greenstein, absorption de Beer-Lambert, ombres (atlas) et gobo dans le faisceau, bruit
// qui dérive. Deux couches par pixel quand ceux de l'image du monde qu'il couvre sont à des
// distances très différentes (bord d'un objet, trous d'un feuillage) : la lumière jusqu'à la
// surface la plus proche, et jusqu'à la plus lointaine. Recomposée en pleine résolution par
// vl_composite.wgsl, chaque pixel prenant la couche de sa propre profondeur. dynlights.wgsl est
// ajouté à la suite.
// D'après la marche volumétrique d'IRLights (MIT, qualet), réécrite pour Minelib.

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
// Profondeur de la scène (pleine résolution) et bruit 3D qui se répète (densité des rayons).
@group(1) @binding(0) var scene_depth: texture_depth_2d;
@group(1) @binding(1) var vnoise: texture_3d<f32>;
@group(1) @binding(2) var vnoise_s: sampler;
// Cases d'écran éclairées (rectangles en NDC) : la passe ne dessine que là.
@group(1) @binding(3) var<storage, read> lit_rects: array<vec4<f32>>;

const VL_PI: f32 = 3.14159265;

// Variante compilée à part sans le bruit des rayons (décoché) : la marche tient dans moins de
// registres. Même image.
override VL_NOISE: bool = true;

/// Pixels de l'image du monde par pixel de la lumière volumétrique (2 : demi-résolution,
/// 4 : quart).
fn vl_scale() -> i32 {
    return i32(clamp(dh.misc.w, 1.0, 8.0));
}

/// Cases éclairées, élargies de 4 pixels de l'image du monde (un pixel de l'image
/// volumétrique au quart de la résolution, deux à la moitié) : la recomposition lit aussi
/// les voisins (dont la profondeur, en alpha) ; une même valeur écrite deux fois là où
/// deux rectangles se recouvrent ne change rien. (L'en-tête des lumières n'est pas lisible
/// ici : il n'est lié qu'aux pixels.)
@vertex
fn vs_tiles(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
    let e = vec2<f32>(8.0 / max(g.screen.x, 1.0), 8.0 / max(g.screen.y, 1.0));
    let r = lit_rects[ii];
    return dl_rect_corner(vi, vec4<f32>(r.x - e.x, r.y + e.y, r.z + e.x, r.w - e.y));
}

/// Diffusion de Henyey-Greenstein (g > 0 : vers l'avant), normalisée à 1/4π.
fn vl_phase(cos_t: f32, g: f32) -> f32 {
    let gg = g * g;
    let den = 1.0 + gg - 2.0 * g * cos_t;
    return (1.0 / (4.0 * VL_PI)) * (1.0 - gg) / (den * sqrt(max(den, 1e-6)));
}

/// Entrée et sortie (t) du rayon `o + t d` dans le cône de sommet `apex`, d'axe `axis`, de
/// demi-angle de cosinus `c`, coupé à la hauteur `h` ; (−1, −1) s'il le manque.
fn vl_ray_cone(o: vec3<f32>, d: vec3<f32>, apex: vec3<f32>, axis: vec3<f32>, c: f32, h: f32) -> vec2<f32> {
    let co = o - apex;
    let cos2 = c * c;
    let dv = dot(d, axis);
    let cov = dot(co, axis);
    let cosq = dot(co, co);
    let inside = cov >= 0.0 && cov <= h && cov * cov >= cosq * cos2;
    let a = dv * dv - cos2;
    let b = dv * cov - dot(d, co) * cos2;
    let cc = cov * cov - cosq * cos2;
    var t1 = -1.0;
    var t2 = -1.0;
    if abs(a) < 1e-6 {
        if abs(b) > 1e-6 {
            t1 = -0.5 * cc / b;
            t2 = t1;
        }
    } else {
        let disc = b * b - a * cc;
        if disc >= 0.0 {
            let sq = sqrt(disc);
            let ta = (-b - sq) / a;
            let tb = (-b + sq) / a;
            t1 = min(ta, tb);
            t2 = max(ta, tb);
        }
    }
    let ax1 = cov + t1 * dv;
    let ax2 = cov + t2 * dv;
    let v1 = t1 > 0.0 && ax1 >= 0.0 && ax1 <= h;
    let v2 = t2 > 0.0 && ax2 >= 0.0 && ax2 <= h;
    var tcap = -1.0;
    var cap_ok = false;
    if abs(dv) > 1e-6 {
        let t = (h - cov) / dv;
        if t > 0.0 {
            let pa = o + t * d - apex;
            let radial2 = dot(pa, pa) - h * h;
            let cap_r2 = h * h * (1.0 - cos2) / max(cos2, 1e-6);
            if radial2 <= cap_r2 {
                tcap = t;
                cap_ok = true;
            }
        }
    }
    if inside {
        var tf = 1e30;
        if v1 {
            tf = min(tf, t1);
        }
        if v2 {
            tf = min(tf, t2);
        }
        if cap_ok {
            tf = min(tf, tcap);
        }
        return select(vec2<f32>(0.0, tf), vec2<f32>(-1.0), tf > 1e29);
    }
    var te = 1e30;
    var tx = -1.0;
    if v1 {
        te = min(te, t1);
        tx = max(tx, t1);
    }
    if v2 {
        te = min(te, t2);
        tx = max(tx, t2);
    }
    if cap_ok {
        te = min(te, tcap);
        tx = max(tx, tcap);
    }
    return select(vec2<f32>(te, tx), vec2<f32>(-1.0), tx <= 0.0 || te > 1e29 || te >= tx);
}

/// Densité du bruit (moyenne ≈ 0,5) au point `p` : deux octaves d'un bruit 3D qui se répète,
/// emporté par le vent.
fn vl_noise(p: vec3<f32>, w2: f32) -> f32 {
    let scale = max(dh.vl_b.y, 0.05);
    let wind = dh.vl_b.w * dh.vl_b.z * vec3<f32>(1.2, 0.4, 0.8);
    let q = (p + wind) / (scale * 32.0);
    let a = textureSampleLevel(vnoise, vnoise_s, q, 0.0).r;
    let b = textureSampleLevel(vnoise, vnoise_s, q * 2.53 + vec3<f32>(0.31, 0.53, 0.47), 0.0).r;
    return mix(a, b, w2);
}

/// Visibilité (1 éclairé, 0 dans l'ombre des murs) de la lumière `L` au point `q` du faisceau.
fn vl_shadow_at(L: DynLight, fr: DlFrame, q: vec3<f32>, range: f32) -> f32 {
    let to = q - L.pos_range.xyz;
    let dist = max(length(to), 1e-4);
    let su = dl_shadow_uv(L, fr, to / dist);
    return select(1.0, textureSampleCompareLevel(shadow_atlas, shadow_cmp, su.xy, dist / range - 0.002), su.z > 0.5);
}

/// Lumière diffusée le long d'un rayon : jusqu'à la surface la plus proche (`near`) et jusqu'à
/// la plus lointaine (`far`) des pixels couverts (égales quand ils sont à la même distance).
struct VlLayers {
    near: vec3<f32>,
    far: vec3<f32>,
};

/// Lumière diffusée vers la caméra `o`, direction `d`, par les lumières de la tuile du pixel
/// `frag` (pleine résolution, image `size`) : jusqu'à `cut_t` (surface la plus proche) et
/// jusqu'à `max_t` (la plus lointaine, ou distance maximale). `cut_t` ≥ `max_t` : une seule
/// couche, la marche d'avant.
fn dyn_volumetric(o: vec3<f32>, d: vec3<f32>, cut_t: f32, max_t: f32, dither: f32, frag: vec2<f32>, size: vec2<f32>) -> VlLayers {
    var result = vec3<f32>(0.0);
    var result_near = vec3<f32>(0.0);
    let count = dh.counts.x;
    if count == 0u || max_t < 0.01 {
        return VlLayers(result, result);
    }
    let base = dl_tile_base(frag, size);
    let words = dl_tile_words(count);
    let flags = dh.vl_c.w;
    let shadows_on = (flags & DL_VL_SHADOWS) != 0u && (flags & DL_SHADOWS) != 0u;
    let noise_on = VL_NOISE && (flags & DL_VL_NOISE) != 0u && dh.vl_b.x > 0.0;
    let step_max = i32(clamp(dh.vl_c.x, 1u, 96u));
    let sh_stride = i32(clamp(dh.vl_c.y, 1u, 8u));
    let nz_stride = i32(clamp(dh.vl_c.z, 1u, 8u));
    let tip_r2 = max(dh.vl_a.w * dh.vl_a.w, 1e-4);
    // force × exp(−x) < 2⁻²⁴ dès que x ≥ 17 + ln(force) (e⁻¹⁷ ≈ 4,1·10⁻⁸ < 2⁻²⁴ ≈ 6·10⁻⁸) ;
    // force nulle : jamais de lueur.
    let tip_cut = select(0.0, tip_r2 * (17.0 + log(max(dh.vl_a.z, 1.0))), dh.vl_a.z != 0.0);
    // Métamorphose : deux tirages du même bruit à des décalages qui changent, mélangés.
    let morph = dh.vl_b.w * dh.misc.y;
    let morph_k = floor(morph);
    let morph_f = morph - morph_k;
    let morph_on = dh.misc.y > 0.0;
    let off0 = fract(vec3<f32>(0.1031, 0.1030, 0.0973) * morph_k * 13.7) * 97.0;
    let off1 = fract(vec3<f32>(0.1031, 0.1030, 0.0973) * (morph_k + 1.0) * 13.7) * 97.0;
    for (var w = 0u; w < words; w++) {
        var bits = dtiles[base + w];
        loop {
            if bits == 0u {
                break;
            }
            let b = firstTrailingBit(bits);
            bits = bits & (bits - 1u);
            let L = dlights[min(w * 32u + b, count - 1u)];
            let col = L.color.rgb * max(L.vol.z, 0.0);
            let lp = L.pos_range.xyz;
            let range = max(L.pos_range.w, 0.001);
            let spot = L.dir_type.w > 0.5;
            var tn = -1.0;
            var tf = -1.0;
            if dot(col, col) > 1e-8 {
                if spot {
                    let tt = vl_ray_cone(o, d, lp, normalize(L.dir_type.xyz), L.cone.x, range);
                    tn = max(tt.x, 0.0);
                    tf = select(-1.0, min(tt.y, max_t), tt.x >= 0.0);
                } else {
                    let oc = o - lp;
                    let bb = dot(oc, d);
                    let c = dot(oc, oc) - range * range;
                    let disc = bb * bb - c;
                    if disc >= 0.0 {
                        let sq = sqrt(disc);
                        tn = max(-bb - sq, 0.0);
                        tf = min(-bb + sq, max_t);
                    }
                }
            }
            let seg = tf - tn;
            if seg > 0.0 {
                // Repère, case de la carte d'ombre et gobo : une fois pour tout le rayon.
                let fr = dl_frame(L);
                let g_hg = clamp(L.vol.x, -0.95, 0.95);
                let ext = max(L.vol.y, 1e-4);
                let axis = normalize(L.dir_type.xyz);
                // Pas : plus fins sur un court segment (au moins 16, jusqu'au maximum dès 24 blocs).
                let steps = clamp(i32(ceil(seg * f32(step_max) / 24.0)), min(16, step_max), step_max);
                // Deux parties quand la surface proche coupe le faisceau : devant elle (`n1`
                // pas, au moins la moitié : la couche proche reste aussi fine), puis derrière
                // jusqu'à la surface lointaine. Même nombre de pas en tout ; une seule partie
                // (la marche d'avant) quand tout est à la même distance.
                let cut = clamp(cut_t, tn, tf);
                var n1 = steps;
                if cut <= tn {
                    n1 = 0;
                } else if cut < tf {
                    n1 = clamp(i32(ceil(f32(steps) * (cut - tn) / seg)), steps / 2, steps - 1);
                }
                let phase_spot = vl_phase(dot(d, -axis), g_hg);
                // Ombres des murs : un échantillon tous les `sh_stride` pas depuis le début de
                // chaque partie, aux mêmes pas pour tous les pixels ; un pas qui éclaire prend
                // l'interpolation des deux qui l'encadrent, lus à la demande (aucun là où le
                // faisceau est trop faible pour compter). Le faisceau s'arrête au mur, sans
                // traîner derrière ni passer avant le premier échantillon.
                let sh_on = shadows_on && L.shadow.x > 0.5;
                var trans = 1.0;
                var acc = vec3<f32>(0.0);
                var acc_n = vec3<f32>(0.0);
                // Avant le premier échantillon, un pas trop faible pour en lire un ne compte pas
                // (au plus 1 % du faisceau, à son bord) : rien ne passe sans avoir été vu.
                var vis = select(1.0, 0.0, sh_on);
                var nz = 0.5;
                for (var part = 0; part < 2; part++) {
                    let n = select(steps - n1, n1, part == 0);
                    if n > 0 && trans >= 0.02 {
                        let step_len = select((tf - cut) / f32(n), (cut - tn) / f32(n), part == 0);
                        let absorb = exp(-ext * step_len);
                        // Octave fine du bruit effacée quand le pas ne la voit plus (scintillement).
                        let w2 = 0.35 * clamp(dh.vl_b.y / (2.53 * step_len), 0.0, 1.0);
                        let last = n - 1;
                        // Intervalle d'ombre [sh_a, sh_b] du pas, 1 / sa longueur, échantillons
                        // lus (`loaded`), et celui du bout du précédent resservi (`b_ok`).
                        // (Dernier bout plus court qu'un intervalle : sa valeur de départ.)
                        var sh_a = 0;
                        var sh_b = select(0, sh_stride, sh_stride <= last);
                        var inv = 1.0 / f32(max(sh_b, 1));
                        var loaded = false;
                        var b_ok = false;
                        var vis_a = 1.0;
                        var vis_b = 1.0;
                        var t = select(cut, tn, part == 0) + step_len * dither;
                        for (var s = 0; s < 96; s++) {
                            if s >= n || trans < 0.02 {
                                break;
                            }
                            if s == sh_b && s > sh_a {
                                sh_a = sh_b;
                                sh_b = select(sh_a, sh_a + sh_stride, sh_a + sh_stride <= last);
                                inv = 1.0 / f32(max(sh_b - sh_a, 1));
                                b_ok = loaded;
                                loaded = false;
                            }
                            let p = o + d * t;
                            let to_l = lp - p;
                            let dist = length(to_l);
                            let l = to_l / max(dist, 1e-6);
                            let fall = max(1.0 - dist / range, 0.0);
                            var att = 0.0;
                            var phase = phase_spot;
                            if spot {
                                let cone = clamp((dot(-l, axis) - L.cone.x) / max(L.cone.y - L.cone.x, 1e-6), 0.0, 1.0);
                                att = fall * cone;
                                if att > 0.0 {
                                    att *= dl_cookie(p, L, fr);
                                }
                            } else {
                                att = fall * fall;
                                phase = vl_phase(dot(d, -l), g_hg);
                            }
                            if att > 0.0 {
                                if sh_on && att > 0.01 {
                                    if !loaded {
                                        if b_ok {
                                            vis_a = vis_b;
                                        } else {
                                            vis_a = vl_shadow_at(L, fr, p - d * (f32(s - sh_a) * step_len), range);
                                        }
                                        vis_b = vis_a;
                                        if sh_b > sh_a {
                                            vis_b = vl_shadow_at(L, fr, p + d * (f32(sh_b - s) * step_len), range);
                                        }
                                        loaded = true;
                                    }
                                    vis = mix(vis_a, vis_b, f32(s - sh_a) * inv);
                                }
                                // Lueur à la source : au-delà de `tip_cut`, force × exp(−d²/r²) reste
                                // sous le demi-écart entre 1,0 et le f32 suivant (2⁻²⁴) : la somme
                                // vaudrait 1,0 exactement. Même valeur, une exponentielle de moins.
                                var tip = 1.0;
                                let tq = dist * dist;
                                if tq < tip_cut {
                                    tip = 1.0 + dh.vl_a.z * exp(-tq / tip_r2);
                                }
                                var ins = att * vis * phase * exp(-ext * dist) * tip;
                                if noise_on && ins > 1e-5 {
                                    if s % nz_stride == 0 {
                                        if morph_on {
                                            nz = mix(vl_noise(p + off0, w2), vl_noise(p + off1, w2), morph_f);
                                        } else {
                                            nz = vl_noise(p, w2);
                                        }
                                    }
                                    ins *= mix(1.0, 2.0 * nz, dh.vl_b.x);
                                }
                                acc += col * ins * (1.0 - absorb) * trans;
                                trans *= absorb;
                            }
                            t += step_len;
                        }
                    }
                    if part == 0 {
                        acc_n = acc;
                    }
                }
                result += acc;
                result_near += acc_n;
            }
        }
    }
    let k = dh.vl_a.x * dh.surf.x;
    return VlLayers(result_near * k, result * k);
}

@fragment
fn fs_vl(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<u32> {
    let dims = vec2<i32>(textureDimensions(scene_depth));
    let sc = vl_scale();
    let base = vec2<i32>(floor(frag.xy)) * sc;
    // Surfaces la plus proche et la plus lointaine des pixels couverts (Z inversé : la plus
    // proche a la plus grande profondeur, le ciel vaut 0).
    var dn = 0.0;
    var df = 1.0;
    for (var y = 0; y < sc; y++) {
        for (var x = 0; x < sc; x++) {
            let q = clamp(base + vec2<i32>(x, y), vec2<i32>(0), dims - 1);
            let z = textureLoad(scene_depth, q, 0);
            dn = max(dn, z);
            df = min(df, z);
        }
    }
    let full = vec2<f32>(dims);
    let mid = vec2<f32>(base) + 0.5 * f32(sc);
    let uv = mid / full;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let cam = g.cam_pos.xyz;
    // Direction du regard tirée des lignes de la matrice (pas d'inverse : voir
    // `dl_view_ray`), et distances des surfaces : proche / profondeur unités de vue.
    let ray = dl_view_ray(g.view_proj, ndc);
    let dir = normalize(ray);
    let unit = length(ray) * g.view_proj[3][2];
    let t_near = select(1e9, unit / dn, dn > 0.0);
    let t_far = select(1e9, unit / df, df > 0.0);
    let reach = dh.vl_a.y;
    // Deux couches seulement au bord d'un objet (surface lointaine plus loin d'un quart au
    // moins : pas sur un sol vu de biais) quand la proche est encore dans la portée des
    // rayons ; sinon la marche s'arrête à la proche, comme avant.
    let cut = min(t_near, reach);
    let max_t = select(cut, min(t_far, reach), t_near < reach && t_far > t_near * 1.25);
    var dither = fract(52.9829189 * fract(0.06711056 * frag.x + 0.00583715 * frag.y));
    if (dh.vl_c.w & DL_DITHER) != 0u {
        dither = fract(dither + dh.misc.x * 0.61803398);
    }
    let c = dyn_volumetric(cam, dir, cut, max_t, dither, mid, full);
    // Chaque couche avec sa profondeur (demi-flottants) : la recomposition prend, pour chaque
    // pixel, celle qui est à sa distance.
    return vec4<u32>(
        pack2x16float(c.near.rg),
        pack2x16float(vec2<f32>(c.near.b, dn)),
        pack2x16float(c.far.rg),
        pack2x16float(vec2<f32>(c.far.b, df)),
    );
}
