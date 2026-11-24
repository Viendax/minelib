// Minelib — (v268) teinte exacte du verre sur les surfaces éclairées par les lumières
// dynamiques : éclairage différé (dyn_deferred.wgsl), ajouté après dynlights.wgsl.
//
// « Fais le même travail que pour les ombres, que ça rende bien la forme d'un bloc et que le
// rendu soit lisse » (pour le verre). La carte de teinte (`fs_tint` de shadow_bake.wgsl) est
// deux à quatre fois moins fine que la carte d'ombre, et une couleur ne se compare pas comme
// une distance : lue telle quelle, la tache colorée au sol avait des bords en escalier et
// baveux, loin de la forme des blocs de verre. Elle ne sert plus ici qu'à savoir s'il y a du
// verre sur le chemin, et à partir d'où (`dl_surface_tint`) : la lumière est alors suivie case
// par case dans la grille de blocs, jusqu'au point éclairé, et filtrée exactement là où elle
// traverse chaque surface de verre, au texel de sa face (même calcul que `shadow_march` :
// Fresnel, puis Beer-Lambert). La tache a la forme exacte des blocs et le motif de leur
// texture ; ses bords sont lissés par l'anticrénelage, comme ceux des blocs.

/// Case de la grille de blocs (celle du raytracing et des cartes d'ombre) : bits 0..11 = bloc
/// (0 : vide) ; 0xFFFF : hors de la grille.
fn dg_voxel(c: vec3<i32>) -> u32 {
    let vox = vec3<i32>(textureDimensions(voxels));
    let l = c - vec3<i32>(g.rt.yzw);
    let inside = all(l >= vec3<i32>(0)) && all(l < vox);
    let raw = textureLoad(voxels, clamp(l, vec3<i32>(0), vox - vec3<i32>(1)), 0).r;
    return select(0xffffu, raw, inside);
}

/// Rayon / boîte : (entrée, sortie ou −1, axe de la face d'entrée). Comme `box_hit` de
/// shadow_bake.wgsl.
fn dg_box_hit(o: vec3<f32>, d: vec3<f32>, mn: vec3<f32>, mx: vec3<f32>) -> vec3<f32> {
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

fn dg_box_lo(b: u32) -> vec3<f32> {
    return vec3<f32>(f32(b & 15u), f32((b >> 4u) & 15u), f32((b >> 8u) & 15u)) / 16.0;
}
fn dg_box_hi(b: u32) -> vec3<f32> {
    return (vec3<f32>(f32((b >> 12u) & 15u), f32((b >> 16u) & 15u), f32((b >> 20u) & 15u)) + 1.0) / 16.0;
}
fn dg_side_dir(s: i32) -> vec3<i32> {
    return select(select(select(vec3<i32>(0, 0, -1), vec3<i32>(0, 0, 1), s == 2), vec3<i32>(-1, 0, 0), s == 1), vec3<i32>(1, 0, 0), s == 0);
}
fn dg_arm_lo(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(lo.z, lo.y, 1.0 - hi.x), vec3<f32>(lo.z, lo.y, lo.x), s == 2), vec3<f32>(1.0 - hi.x, lo.y, lo.z), s == 1), lo, s == 0);
}
fn dg_arm_hi(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(hi.z, hi.y, 1.0 - lo.x), vec3<f32>(hi.z, hi.y, hi.x), s == 2), vec3<f32>(1.0 - lo.x, hi.y, hi.z), s == 1), hi, s == 0);
}

/// Une vitre se relie-t-elle à la case `c` ? (comme `post_link` de shadow_bake.wgsl)
fn dg_post_link(c: vec3<i32>) -> bool {
    let raw = dg_voxel(c);
    let id = raw & 4095u;
    let kind = textureLoad(palette, i32(id) + 4096, 0).w & 3u;
    let torch = (textureLoad(palette, i32(id), 0).w >> 8u) & 7u;
    return raw != 0xffffu && id != 0u && torch == 0u && (kind == 0u || kind == 2u);
}

/// Plaque d'une vitre (poteau et bras vers ses voisines) touchée en premier : x = distance
/// (< 0 : rien), y = axe de la face. Comme `shape_hit` de shadow_bake.wgsl.
fn dg_shape_hit(o: vec3<f32>, d: vec3<f32>, cell: vec3<i32>, shp: vec4<u32>, t_in: f32) -> vec2<f32> {
    let lo = o - vec3<f32>(cell);
    let b0 = shp.x | ((shp.y & 255u) << 16u);
    let b1 = (shp.y >> 8u) | (shp.z << 8u);
    let kind = shp.w & 3u;
    let post = (shp.w >> 2u) & 3u;
    var link = vec4<bool>(false);
    if kind == 2u {
        link = vec4<bool>(dg_post_link(cell + dg_side_dir(0)), dg_post_link(cell + dg_side_dir(1)), dg_post_link(cell + dg_side_dir(2)), dg_post_link(cell + dg_side_dir(3)));
        if post == 2u && !any(link) {
            link = vec4<bool>(true);
        }
    }
    var best = vec2<f32>(-1.0, 0.0);
    var bt = 1e9;
    for (var k = 0; k < 10; k++) {
        var mn = dg_box_lo(b0);
        var mx = dg_box_hi(b0);
        var used = k == 0;
        if k == 1 {
            mn = dg_box_lo(b1);
            mx = dg_box_hi(b1);
            used = kind == 1u && b1 != 0u;
        } else if k >= 2 {
            let side = (k - 2) / 2;
            let second = (k - 2) % 2 == 1;
            let lift = vec3<f32>(0.0, select(0.0, 0.375, second), 0.0);
            mn = dg_arm_lo(dg_box_lo(b1), dg_box_hi(b1), side) + lift;
            mx = dg_arm_hi(dg_box_lo(b1), dg_box_hi(b1), side) + lift;
            used = kind == 2u && link[side] && (!second || post == 0u);
        }
        if used {
            let h = dg_box_hit(lo, d, mn, mx);
            if h.y >= 0.0 && h.x >= t_in - 1e-3 && h.x < bt {
                bt = h.x;
                best = vec2<f32>(h.x, h.z);
            }
        }
    }
    return best;
}

/// Ce que laisse passer une surface de verre traversée (`glass_pass` de shadow_bake.wgsl) :
/// Fresnel (Schlick, 4 % de face), puis la couche teintée (Beer-Lambert, plus épaisse en biais).
fn dg_pass(c: vec4<f32>, cosi: f32) -> vec3<f32> {
    let mu = clamp(cosi, 0.0, 1.0);
    let fresnel = 1.0 - (0.04 + 0.96 * pow(1.0 - mu, 5.0));
    let depth = clamp(c.a, 0.0, 1.0) / max(mu, 0.2);
    return fresnel * pow(clamp(c.rgb, vec3<f32>(1e-4), vec3<f32>(1.0)), vec3<f32>(depth));
}

/// Filtre de la face d'un bloc de verre (`pal` : ses textures) traversée au temps `t`,
/// perpendiculaire à l'axe `axis` : le texel de la face à cet endroit (`glass_face` de
/// shadow_bake.wgsl). `exit` : la face par où la lumière sort de la case.
fn dg_face(o: vec3<f32>, d: vec3<f32>, t: f32, axis: i32, step: vec3<i32>, pal: vec4<u32>, exit: bool) -> vec3<f32> {
    let p = o + d * t;
    var uv = vec2<f32>(p.x, p.z);
    var layer = select(pal.z, pal.x, (step.y < 0) != exit);
    var cosi = abs(d.y);
    if axis == 0 {
        uv = vec2<f32>(p.z, -p.y);
        layer = pal.y;
        cosi = abs(d.x);
    } else if axis == 2 {
        uv = vec2<f32>(p.x, -p.y);
        layer = pal.y;
        cosi = abs(d.z);
    }
    let c = textureSampleLevel(blocks, block_sampler, fract(uv), i32(layer), 0.0);
    return dg_pass(c, cosi);
}

/// Filtre des verres que traverse la lumière partie de `o` jusqu'au point éclairé `q`, suivie
/// case par case depuis la distance `t_start` (avant : de l'air, d'après la carte de teinte ;
/// une case de verre au départ : depuis la lumière). `own` : la lumière est dans un bloc
/// lumineux, sa propre case ne compte pas. w = 1 : trajet connu ; 0 : il sort de la grille de
/// blocs (la carte de teinte décide).
fn dl_glass_exact(o: vec3<f32>, q: vec3<f32>, t_start: f32, own: bool) -> vec4<f32> {
    let to = q - o;
    let max_t = length(to);
    if max_t < 1e-4 {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    let d = to / max_t;
    let start = vec3<i32>(floor(o));
    var t0 = clamp(t_start, 0.0, max_t);
    if t0 > 0.0 {
        let raw = dg_voxel(vec3<i32>(floor(o + d * t0)));
        let id = raw & 4095u;
        if raw != 0xffffu && id != 0u && ((textureLoad(palette, i32(id), 0).w >> 11u) & 1u) == 1u {
            t0 = 0.0;
        }
    }
    let s = o + d * t0;
    var cell = vec3<i32>(floor(s));
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    // Temps (depuis `o`) des prochaines limites de case, comme si la marche était partie de `o`.
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - o) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var axis = 1;
    var t = t0;
    var filt = vec3<f32>(1.0);
    var in_glass = false;
    var glass_pal = vec4<u32>(0u);
    // La limite de case passée juste après le point éclairé (voir la fin).
    var end_t = -1.0;
    var end_axis = 1;
    // Au plus ~√3 cases par bloc parcouru (borne lue à l'exécution : pas de déroulement).
    let n = i32(ceil((max_t - t0) * 1.8)) + 3;
    for (var i = 0; i < 256; i++) {
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
        if t > max_t {
            end_t = t;
            end_axis = axis;
            break;
        }
        let raw = dg_voxel(cell);
        if raw == 0xffffu {
            return vec4<f32>(filt, 0.0);
        }
        let id = raw & 4095u;
        let at_start = all(cell == start);
        var pal = vec4<u32>(0u);
        var shp = vec4<u32>(0u);
        if id != 0u {
            pal = textureLoad(palette, i32(id), 0);
            shp = textureLoad(palette, i32(id) + 4096, 0);
        }
        let glass = id != 0u && !(own && at_start) && ((pal.w >> 11u) & 1u) == 1u;
        let glass_cube = glass && (shp.w & 3u) != 2u;
        // Sortie d'un amas de blocs de verre collés (pas de face entre eux) : sa surface, vue de
        // dedans.
        if in_glass && !glass_cube {
            filt *= dg_face(o, d, t, axis, step, glass_pal, true);
            in_glass = false;
        }
        if glass {
            if (shp.w & 3u) == 2u {
                // Vitre : les deux faces de sa plaque.
                let sh = dg_shape_hit(o, d, cell, shp, t);
                if sh.x >= 0.0 && sh.x <= max_t {
                    let f = dg_face(o, d, sh.x, i32(sh.y), step, pal, false);
                    filt *= f * f;
                }
            } else {
                // Entrée dans un bloc de verre (pas celle d'une lumière qui y est déjà).
                if !in_glass && !at_start {
                    filt *= dg_face(o, d, t, axis, step, pal, false);
                }
                in_glass = true;
                glass_pal = pal;
            }
        }
    }
    // Le point éclairé touche le verre (un bloc de verre posé sur lui : le point, à peine
    // au-dessus de sa face, est encore dans la case du verre) : la face par où la lumière en
    // sort, juste après.
    if in_glass && end_t >= 0.0 {
        filt *= dg_face(o, d, end_t, end_axis, step, glass_pal, true);
    }
    return vec4<f32>(filt, 1.0);
}
