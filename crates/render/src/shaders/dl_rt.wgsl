// ---------------------------------------------------------------- ombres par lancer de rayons
// Essai (réglage « Ombres par lancer de rayons ») : l'ombre d'une lumière dynamique sur un
// pixel, par un rayon du pixel vers un point de l'ampoule, case par case dans la grille de
// blocs (la même que pour le calcul des cartes, shadow_bake.wgsl), au lieu de la carte
// d'ombre. Un rayon par pixel et par lumière, vers un point de l'ampoule qui change d'une
// image à l'autre : la pénombre est lissée par l'anticrénelage temporel. Hors de la grille,
// la carte d'ombre reprend. Ajouté à la suite de dynlights.wgsl pour l'éclairage différé
// seulement (les autres shaders ont `dl_rt_shadow` de dl_rt_off.wgsl).

const RT_VOX: vec3<i32> = vec3<i32>(128, 64, 128);

@group(0) @binding(1) var rt_blocks: texture_2d_array<f32>;
@group(0) @binding(2) var rt_sampler: sampler;
@group(0) @binding(3) var rt_voxels: texture_3d<u32>;
@group(0) @binding(4) var rt_palette: texture_1d<u32>;

/// Case de la grille : bits 0..11 = bloc (0 = vide) ; 0xFFFF = hors de la grille.
fn rt_voxel(c: vec3<i32>) -> u32 {
    let l = c - dh.vox.xyz;
    let inside = all(l >= vec3<i32>(0)) && all(l < RT_VOX);
    let raw = textureLoad(rt_voxels, clamp(l, vec3<i32>(0), RT_VOX - vec3<i32>(1)), 0).r;
    return select(0xffffu, raw, inside);
}

/// Rayon / boîte : (entrée, sortie ou −1, axe de la face d'entrée).
fn rt_box_hit(o: vec3<f32>, d: vec3<f32>, mn: vec3<f32>, mx: vec3<f32>) -> vec3<f32> {
    let sd = select(vec3<f32>(-1.0), vec3<f32>(1.0), d >= vec3<f32>(0.0));
    let inv = sd / max(abs(d), vec3<f32>(1e-8));
    let a = (mn - o) * inv;
    let b = (mx - o) * inv;
    let t0 = min(a, b);
    let t1 = max(a, b);
    let tn = max(max(t0.x, t0.y), t0.z);
    let tf = min(min(t1.x, t1.y), t1.z);
    var axis = 0.0;
    if t0.y >= t0.x && t0.y >= t0.z {
        axis = 1.0;
    } else if t0.z >= t0.x && t0.z >= t0.y {
        axis = 2.0;
    }
    return vec3<f32>(tn, select(-1.0, tf, tf >= max(tn, 0.0)), axis);
}

fn rt_box_lo(b: u32) -> vec3<f32> {
    return vec3<f32>(f32(b & 15u), f32((b >> 4u) & 15u), f32((b >> 8u) & 15u)) / 16.0;
}
fn rt_box_hi(b: u32) -> vec3<f32> {
    return (vec3<f32>(f32((b >> 12u) & 15u), f32((b >> 16u) & 15u), f32((b >> 20u) & 15u)) + 1.0) / 16.0;
}
fn rt_side_dir(s: i32) -> vec3<i32> {
    return select(select(select(vec3<i32>(0, 0, -1), vec3<i32>(0, 0, 1), s == 2), vec3<i32>(-1, 0, 0), s == 1), vec3<i32>(1, 0, 0), s == 0);
}
fn rt_arm_lo(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(lo.z, lo.y, 1.0 - hi.x), vec3<f32>(lo.z, lo.y, lo.x), s == 2), vec3<f32>(1.0 - hi.x, lo.y, lo.z), s == 1), lo, s == 0);
}
fn rt_arm_hi(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(hi.z, hi.y, 1.0 - lo.x), vec3<f32>(hi.z, hi.y, hi.x), s == 2), vec3<f32>(1.0 - lo.x, hi.y, hi.z), s == 1), hi, s == 0);
}

/// Un poteau se relie-t-il à la case `c` ? (comme dans world.wgsl)
fn rt_post_link(c: vec3<i32>) -> bool {
    let raw = rt_voxel(c);
    let id = raw & 4095u;
    let kind = textureLoad(rt_palette, i32(id) + 4096, 0).w & 3u;
    let torch = (textureLoad(rt_palette, i32(id), 0).w >> 8u) & 7u;
    return raw != 0xffffu && id != 0u && torch == 0u && (kind == 0u || kind == 2u);
}

/// Boîte la plus proche touchée dans un bloc non plein : x = distance (< 0 : rien),
/// y = axe de la face (même calcul que shadow_bake.wgsl).
fn rt_shape_hit(o: vec3<f32>, d: vec3<f32>, cell: vec3<i32>, shp: vec4<u32>, t_in: f32) -> vec2<f32> {
    let lo = o - vec3<f32>(cell);
    let b0 = shp.x | ((shp.y & 255u) << 16u);
    let b1 = (shp.y >> 8u) | (shp.z << 8u);
    let kind = shp.w & 3u;
    let post = (shp.w >> 2u) & 3u;
    var link = vec4<bool>(false);
    if kind == 2u {
        link = vec4<bool>(rt_post_link(cell + rt_side_dir(0)), rt_post_link(cell + rt_side_dir(1)), rt_post_link(cell + rt_side_dir(2)), rt_post_link(cell + rt_side_dir(3)));
        if post == 2u && !any(link) {
            link = vec4<bool>(true);
        }
    }
    var best = vec2<f32>(-1.0, 0.0);
    var bt = 1e9;
    for (var k = 0; k < 10; k++) {
        var mn = rt_box_lo(b0);
        var mx = rt_box_hi(b0);
        var used = k == 0;
        if k == 1 {
            mn = rt_box_lo(b1);
            mx = rt_box_hi(b1);
            used = kind == 1u && b1 != 0u;
        } else if k >= 2 {
            let side = (k - 2) / 2;
            let second = (k - 2) % 2 == 1;
            let lift = vec3<f32>(0.0, select(0.0, 0.375, second), 0.0);
            mn = rt_arm_lo(rt_box_lo(b1), rt_box_hi(b1), side) + lift;
            mx = rt_arm_hi(rt_box_lo(b1), rt_box_hi(b1), side) + lift;
            used = kind == 2u && link[side] && (!second || post == 0u);
        }
        if used {
            let h = rt_box_hit(lo, d, mn, mx);
            if h.y >= 0.0 && h.x >= t_in - 1e-3 && h.x < bt {
                bt = h.x;
                best = vec2<f32>(h.x, h.z);
            }
        }
    }
    return best;
}

/// Un bloc arrête-t-il le rayon `o` → `o + d × max_t` ? La case `stop` (celle de la lumière)
/// ne compte pas. −1 : sorti de la grille avant (inconnu).
fn rt_blocked(o: vec3<f32>, d: vec3<f32>, max_t: f32, stop: vec3<i32>) -> f32 {
    var cell = vec3<i32>(floor(o));
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - o) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var axis = 1;
    var t = 0.0;
    // Au plus ~√3 cases par bloc parcouru (borne lue à l'exécution : pas de déroulement).
    let n = i32(ceil(max_t * 1.8)) + 3;
    for (var i = 0; i < 512; i++) {
        if i >= n {
            break;
        }
        if i > 0 {
            if t_max.x < t_max.y && t_max.x < t_max.z {
                t = t_max.x;
                cell.x += step.x;
                t_max.x += inv.x;
                axis = 0;
            } else if t_max.y < t_max.z {
                t = t_max.y;
                cell.y += step.y;
                t_max.y += inv.y;
                axis = 1;
            } else {
                t = t_max.z;
                cell.z += step.z;
                t_max.z += inv.z;
                axis = 2;
            }
        }
        if t > max_t || all(cell == stop) {
            return 0.0;
        }
        let raw = rt_voxel(cell);
        if raw == 0xffffu {
            return -1.0;
        }
        let id = raw & 4095u;
        if id != 0u {
            let pal = textureLoad(rt_palette, i32(id), 0);
            let torch = (pal.w >> 8u) & 7u;
            let shp = textureLoad(rt_palette, i32(id) + 4096, 0);
            let partial = (shp.w & 3u) != 0u;
            var ht = t;
            var hax = axis;
            var hit = torch == 0u;
            if hit && partial {
                let sh = rt_shape_hit(o, d, cell, shp, t);
                hit = sh.x >= 0.0 && sh.x <= max_t;
                ht = sh.x;
                hax = i32(sh.y);
            }
            // Trous de la texture (feuillage, verre, rail, échelle) : la lumière passe.
            let cutout = partial || ((shp.w >> 4u) & 1u) != 0u;
            if hit && cutout {
                let p = o + d * ht;
                var uv = vec2<f32>(p.x, p.z);
                var layer = select(pal.z, pal.x, step.y < 0);
                if hax == 0 {
                    uv = vec2<f32>(p.z, -p.y);
                    layer = pal.y;
                } else if hax == 2 {
                    uv = vec2<f32>(p.x, -p.y);
                    layer = pal.y;
                }
                let a = textureSampleLevel(rt_blocks, rt_sampler, fract(uv), i32(layer), 0.0).a;
                hit = a >= 0.5;
            }
            if hit {
                return 1.0;
            }
        }
    }
    return 0.0;
}

/// Visibilité de la lumière `L` depuis `p` (normale `n`) par un rayon vers un point de son
/// ampoule (pénombre au fil des images) : 0 caché, 1 éclairé, −1 hors de la grille (la carte
/// d'ombre décide).
fn dl_rt_shadow(L: DynLight, p: vec3<f32>, n: vec3<f32>, noise: f32, frag: vec2<f32>) -> f32 {
    let lp = L.pos_range.xyz;
    let q = p + n * 0.02;
    // Pixel et lumière dans la grille (sinon le rayon en sortirait sans rien savoir) : la
    // carte d'ombre décide, sans marche inutile.
    let lc = vec3<i32>(floor(lp)) - dh.vox.xyz;
    let qc = vec3<i32>(floor(q)) - dh.vox.xyz;
    if dh.vox.w == 0 || any(min(lc, qc) < vec3<i32>(0)) || any(max(lc, qc) >= RT_VOX) {
        return -1.0;
    }
    let to = lp - q;
    let dist = length(to);
    if dist < 1e-3 {
        return 1.0;
    }
    let dir = to / dist;
    // Point de l'ampoule (disque face au pixel, même taille que pour l'ombre douce).
    let bulb = select(dh.sh.x, L.cone.w, L.cone.w > 0.0);
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(dir.y) > 0.9);
    let t1 = normalize(cross(dir, up));
    let t2 = cross(dir, t1);
    let n2 = fract(52.9829189 * fract(0.00583715 * frag.x + 0.06711056 * frag.y) + noise * 0.618034);
    let r = bulb * sqrt(noise);
    let a = 6.2831853 * n2;
    let aim = lp + (t1 * cos(a) + t2 * sin(a)) * r;
    let tv = aim - q;
    let tl = length(tv);
    let b = rt_blocked(q, tv / max(tl, 1e-6), max(tl - 0.05, 0.0), vec3<i32>(floor(lp)));
    return select(1.0 - b, -1.0, b < 0.0);
}
