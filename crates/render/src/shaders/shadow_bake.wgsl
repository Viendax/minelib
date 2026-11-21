// Minelib — cartes d'ombre des lumières dynamiques, calculées dans la grille de blocs du
// raytracing (voir lights.rs). Une case de l'atlas par projecteur, six (un cube) par lumière
// ponctuelle. Chaque texel lance un rayon depuis la lumière, case par case (DDA), et garde la
// distance du premier bloc touché (rapportée à la portée) : formes des blocs non pleins
// (dalles, escaliers, clôtures…) et trous des textures (feuillage, verre, rails) compris.

const VOX: vec3<i32> = vec3<i32>(128, 64, 128);

struct BakeJob {
    // xyz = position de la lumière, w = portée.
    pos_range: vec4<f32>,
    // Repère de la case : avant (w = tangente du demi-angle), droite (w = 1 : la lumière est
    // dans un bloc lumineux, sa propre case ne fait pas d'ombre), haut.
    fwd: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
};

struct Bake {
    // xyz = coin bas de la grille de blocs (monde).
    origin: vec4<i32>,
};

@group(0) @binding(0) var<storage, read> jobs: array<BakeJob>;
@group(0) @binding(1) var<uniform> bk: Bake;
@group(0) @binding(2) var voxels: texture_3d<u32>;
@group(0) @binding(3) var palette: texture_1d<u32>;
@group(0) @binding(4) var blocks: texture_2d_array<f32>;
@group(0) @binding(5) var block_sampler: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) job: u32,
};

/// Triangle qui couvre la case (le viewport est réglé sur elle) ; une instance par travail.
@vertex
fn vs_bake(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let ndc = p * 2.0 - 1.0;
    var o: VOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    o.job = ii;
    return o;
}

/// Case de la grille : bits 0..11 = bloc (0 = vide) ; 0xFFFF = hors de la grille.
fn voxel(c: vec3<i32>) -> u32 {
    let l = c - bk.origin.xyz;
    let inside = all(l >= vec3<i32>(0)) && all(l < VOX);
    let raw = textureLoad(voxels, clamp(l, vec3<i32>(0), VOX - vec3<i32>(1)), 0).r;
    return select(0xffffu, raw, inside);
}

/// Rayon / boîte : (entrée, sortie ou −1, axe de la face d'entrée).
fn box_hit(o: vec3<f32>, d: vec3<f32>, mn: vec3<f32>, mx: vec3<f32>) -> vec3<f32> {
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

fn box_lo(b: u32) -> vec3<f32> {
    return vec3<f32>(f32(b & 15u), f32((b >> 4u) & 15u), f32((b >> 8u) & 15u)) / 16.0;
}
fn box_hi(b: u32) -> vec3<f32> {
    return (vec3<f32>(f32((b >> 12u) & 15u), f32((b >> 16u) & 15u), f32((b >> 20u) & 15u)) + 1.0) / 16.0;
}

fn side_dir(s: i32) -> vec3<i32> {
    return select(select(select(vec3<i32>(0, 0, -1), vec3<i32>(0, 0, 1), s == 2), vec3<i32>(-1, 0, 0), s == 1), vec3<i32>(1, 0, 0), s == 0);
}
fn arm_lo(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(lo.z, lo.y, 1.0 - hi.x), vec3<f32>(lo.z, lo.y, lo.x), s == 2), vec3<f32>(1.0 - hi.x, lo.y, lo.z), s == 1), lo, s == 0);
}
fn arm_hi(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(hi.z, hi.y, 1.0 - lo.x), vec3<f32>(hi.z, hi.y, hi.x), s == 2), vec3<f32>(1.0 - lo.x, hi.y, hi.z), s == 1), hi, s == 0);
}

/// Un poteau se relie-t-il à la case `c` ? (comme dans world.wgsl)
fn post_link(c: vec3<i32>) -> bool {
    let raw = voxel(c);
    let id = raw & 4095u;
    let kind = textureLoad(palette, i32(id) + 4096, 0).w & 3u;
    let torch = (textureLoad(palette, i32(id), 0).w >> 8u) & 7u;
    return raw != 0xffffu && id != 0u && torch == 0u && (kind == 0u || kind == 2u);
}

/// Boîte la plus proche touchée dans un bloc non plein : x = distance (< 0 : rien),
/// y = axe de la face (même calcul que `shape_hit` de world.wgsl).
fn shape_hit(o: vec3<f32>, d: vec3<f32>, cell: vec3<i32>, shp: vec4<u32>, t_in: f32) -> vec2<f32> {
    let lo = o - vec3<f32>(cell);
    let b0 = shp.x | ((shp.y & 255u) << 16u);
    let b1 = (shp.y >> 8u) | (shp.z << 8u);
    let kind = shp.w & 3u;
    let post = (shp.w >> 2u) & 3u;
    var link = vec4<bool>(false);
    if kind == 2u {
        link = vec4<bool>(post_link(cell + side_dir(0)), post_link(cell + side_dir(1)), post_link(cell + side_dir(2)), post_link(cell + side_dir(3)));
        if post == 2u && !any(link) {
            link = vec4<bool>(true);
        }
    }
    var best = vec2<f32>(-1.0, 0.0);
    var bt = 1e9;
    for (var k = 0; k < 10; k++) {
        var mn = box_lo(b0);
        var mx = box_hi(b0);
        var used = k == 0;
        if k == 1 {
            mn = box_lo(b1);
            mx = box_hi(b1);
            used = kind == 1u && b1 != 0u;
        } else if k >= 2 {
            let side = (k - 2) / 2;
            let second = (k - 2) % 2 == 1;
            let lift = vec3<f32>(0.0, select(0.0, 0.375, second), 0.0);
            mn = arm_lo(box_lo(b1), box_hi(b1), side) + lift;
            mx = arm_hi(box_lo(b1), box_hi(b1), side) + lift;
            used = kind == 2u && link[side] && (!second || post == 0u);
        }
        if used {
            let h = box_hit(lo, d, mn, mx);
            if h.y >= 0.0 && h.x >= t_in - 1e-3 && h.x < bt {
                bt = h.x;
                best = vec2<f32>(h.x, h.z);
            }
        }
    }
    return best;
}

/// Plante en croix (deux plans en diagonale de la case, de 0,1 à 0,9, sur toute sa
/// hauteur, comme `emit_cross_at`) : distance du premier point opaque de sa texture sur le
/// trajet (< 0 : la lumière passe). Mêmes coordonnées de texture que ses faces dans le
/// monde (x et −y du point, couche des côtés).
fn cross_hit(o: vec3<f32>, d: vec3<f32>, cell: vec3<i32>, t_in: f32, layer: u32) -> f32 {
    let lo = o - vec3<f32>(cell);
    // Plan x = z, puis plan x + z = 1 (coordonnées de la case).
    let ta = select(-1.0, -(lo.x - lo.z) / (d.x - d.z), abs(d.x - d.z) > 1e-6);
    let tb = select(-1.0, (1.0 - lo.x - lo.z) / (d.x + d.z), abs(d.x + d.z) > 1e-6);
    var best = -1.0;
    for (var k = 0; k < 2; k++) {
        // Le plus proche d'abord ; l'autre si le premier est transparent à cet endroit.
        let first = select(tb, ta, ta <= tb);
        let second = select(ta, tb, ta <= tb);
        let t = select(second, first, k == 0);
        if t >= t_in - 1e-4 && best < 0.0 {
            let q = lo + d * t;
            if q.x >= 0.1 && q.x <= 0.9 && q.y >= 0.0 && q.y <= 1.0 {
                let p = o + d * t;
                let a = textureSampleLevel(blocks, block_sampler, fract(vec2<f32>(p.x, -p.y)), i32(layer), 0.0).a;
                if a >= 0.5 {
                    best = t;
                }
            }
        }
    }
    return best;
}

/// Distance du premier bloc qui arrête la lumière partie de `o` vers `d` (au plus `max_t`).
fn shadow_march(o: vec3<f32>, d: vec3<f32>, max_t: f32, skip_start: bool) -> f32 {
    var cell = vec3<i32>(floor(o));
    let start = cell;
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - o) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var axis = 1;
    var t = 0.0;
    var result = max_t;
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
        if t > max_t {
            break;
        }
        let raw = voxel(cell);
        if raw == 0xffffu {
            break; // hors de la grille : rien de connu au-delà
        }
        let id = raw & 4095u;
        let at_start = all(cell == start);
        let own = skip_start && at_start;
        if id != 0u && !own {
            let pal = textureLoad(palette, i32(id), 0);
            let torch = (pal.w >> 8u) & 7u;
            let shp = textureLoad(palette, i32(id) + 4096, 0);
            let kind = shp.w & 3u;
            let partial = kind != 0u && kind != 3u;
            var ht = t;
            var hax = axis;
            // (v222) Lumière posée dans un bloc non plein à trous (feuillage, verre clair) ou
            // dans une plante : elle en sort, son propre bloc ne l'arrête pas. Avant, chaque
            // rayon y lisait la texture au point de départ (la lumière elle-même), le même
            // pour tous : selon l'endroit de la case, toute la lumière passait ou aucune.
            // (Dalles, escaliers, clôtures : leurs boîtes comptent ; celle où est la
            // lumière ne l'arrête pas, voir `shape_hit`.)
            let holed = kind == 3u || (kind == 0u && ((shp.w >> 4u) & 1u) != 0u);
            var hit = torch == 0u && !(at_start && holed);
            if hit && kind == 3u {
                // Plante : ses deux plans, trous de la texture compris.
                ht = cross_hit(o, d, cell, t, pal.y);
                hit = ht >= 0.0;
            } else if hit && partial {
                let sh = shape_hit(o, d, cell, shp, t);
                hit = sh.x >= 0.0;
                ht = sh.x;
                hax = i32(sh.y);
            }
            // Trous de la texture (feuillage, verre, rail, échelle) : la lumière passe.
            // (Plante : déjà fait par `cross_hit`.)
            let cutout = partial || (kind == 0u && ((shp.w >> 4u) & 1u) != 0u);
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
                let a = textureSampleLevel(blocks, block_sampler, fract(uv), i32(layer), 0.0).a;
                hit = a >= 0.5;
            }
            if hit {
                result = max(ht, 0.0);
                break;
            }
        }
    }
    return result;
}

@fragment
fn fs_bake(i: VOut) -> @builtin(frag_depth) f32 {
    let j = jobs[i.job];
    let ndc = vec2<f32>(i.uv.x * 2.0 - 1.0, 1.0 - i.uv.y * 2.0);
    let d = normalize(j.fwd.xyz + (j.right.xyz * ndc.x + j.up.xyz * ndc.y) * j.fwd.w);
    let range = max(j.pos_range.w, 0.01);
    let t = shadow_march(j.pos_range.xyz, d, range, j.right.w > 0.5);
    return clamp(t / range, 0.0, 1.0);
}
