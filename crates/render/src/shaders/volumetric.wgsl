// Minelib — lumière volumétrique des lumières dynamiques (rayons dans la brume, la fumée, la
// poussière). Calculée en demi-résolution : pour chaque pixel, marche le long du regard dans
// le volume de chaque lumière de sa tuile (sphère ou cône), diffusion de Henyey-Greenstein,
// absorption de Beer-Lambert, ombres (atlas) et gobo dans le faisceau, bruit qui dérive.
// Recomposée en pleine résolution par vl_composite.wgsl. dynlights.wgsl est ajouté à la suite.
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

const VL_PI: f32 = 3.14159265;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
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

/// Lumière diffusée vers la caméra `o`, direction `d`, jusqu'à `max_t` (surface ou distance
/// maximale) par les lumières de la tuile du pixel `frag` (pleine résolution, image `size`).
fn dyn_volumetric(o: vec3<f32>, d: vec3<f32>, max_t: f32, dither: f32, frag: vec2<f32>, size: vec2<f32>) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    let count = dh.counts.x;
    if count == 0u || max_t < 0.01 {
        return result;
    }
    let base = dl_tile_base(frag, size);
    let words = min(dh.counts.w, 8u);
    let flags = dh.vl_c.w;
    let shadows_on = (flags & DL_VL_SHADOWS) != 0u && (flags & DL_SHADOWS) != 0u;
    let noise_on = (flags & DL_VL_NOISE) != 0u && dh.vl_b.x > 0.0;
    let step_max = i32(clamp(dh.vl_c.x, 1u, 96u));
    let sh_stride = i32(clamp(dh.vl_c.y, 1u, 8u));
    let nz_stride = i32(clamp(dh.vl_c.z, 1u, 8u));
    let tip_r2 = max(dh.vl_a.w * dh.vl_a.w, 1e-4);
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
                let g_hg = clamp(L.vol.x, -0.95, 0.95);
                let ext = max(L.vol.y, 1e-4);
                let axis = normalize(L.dir_type.xyz);
                // Pas : plus fins sur un court segment (au moins 16, jusqu'au maximum dès 24 blocs).
                let steps = clamp(i32(ceil(seg * f32(step_max) / 24.0)), min(16, step_max), step_max);
                let step_len = seg / f32(steps);
                let absorb = exp(-ext * step_len);
                let phase_spot = vl_phase(dot(d, -axis), g_hg);
                // Octave fine du bruit effacée quand le pas ne la voit plus (scintillement).
                let w2 = 0.35 * clamp(dh.vl_b.y / (2.53 * step_len), 0.0, 1.0);
                var trans = 1.0;
                var acc = vec3<f32>(0.0);
                var vis = 1.0;
                var nz = 0.5;
                var t = tn + step_len * dither;
                for (var s = 0; s < 96; s++) {
                    if s >= steps || trans < 0.02 {
                        break;
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
                            att *= dl_cookie(p, L);
                        }
                    } else {
                        att = fall * fall;
                        phase = vl_phase(dot(d, -l), g_hg);
                    }
                    if att > 0.0 {
                        if shadows_on && L.shadow.x > 0.5 && att > 0.01 && s % sh_stride == 0 {
                            let su = dl_shadow_uv(L, -l);
                            vis = select(1.0, textureSampleCompareLevel(shadow_atlas, shadow_cmp, su.xy, dist / range - 0.002), su.z > 0.5);
                        }
                        let tip = 1.0 + dh.vl_a.z * exp(-(dist * dist) / tip_r2);
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
                result += acc;
            }
        }
    }
    return result * dh.vl_a.x * dh.surf.x;
}

@fragment
fn fs_vl(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(scene_depth));
    let base = vec2<i32>(floor(frag.xy)) * 2;
    // Surface la plus proche des quatre pixels : le faisceau s'arrête au premier obstacle
    // (la recomposition choisit ensuite selon la profondeur de chaque pixel).
    var depth = 0.0;
    for (var k = 0; k < 4; k++) {
        let q = clamp(base + vec2<i32>(k & 1, k >> 1), vec2<i32>(0), dims - 1);
        depth = max(depth, textureLoad(scene_depth, q, 0));
    }
    let full = vec2<f32>(dims);
    let uv = (vec2<f32>(base) + 1.0) / full;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let cam = g.cam_pos.xyz;
    // Direction du regard : un point sur le plan proche (profondeur 1, Z inversé).
    let pn = g.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let dir = normalize(pn.xyz / pn.w - cam);
    var scene = 1e9;
    if depth > 0.0 {
        let pw = g.inv_view_proj * vec4<f32>(ndc, depth, 1.0);
        scene = length(pw.xyz / pw.w - cam);
    }
    let max_t = min(scene, dh.vl_a.y);
    var dither = fract(52.9829189 * fract(0.06711056 * frag.x + 0.00583715 * frag.y));
    if (dh.vl_c.w & DL_DITHER) != 0u {
        dither = fract(dither + dh.misc.x * 0.61803398);
    }
    let c = dyn_volumetric(cam, dir, max_t, dither, vec2<f32>(base) + 1.0, full);
    return vec4<f32>(c, depth);
}
