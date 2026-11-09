// Minelib — shaders du monde (blocs), du ciel et du réticule.
// Les couleurs sont écrites en sRGB puis converties en linéaire ; la surface est sRGB.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,   // xyz = position caméra, w = temps (s)
    fog: vec4<f32>,       // x = début, y = fin, z = ratio largeur/hauteur
    sun: vec4<f32>,       // xyz = direction vers le soleil, w = lumière ambiante
    sel_min: vec4<f32>,   // boîte du bloc visé : xyz = coin min, w = 1 si un bloc est visé
    sel_max: vec4<f32>,   // xyz = coin max
    light: vec4<f32>,     // x = lumière du jour (0 nuit .. 1 jour), y = force des torches
    // Anticrénelage temporel : matrices sans décalage sous-pixel (image courante et
    // précédente) pour les vecteurs de mouvement.
    cur_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    screen: vec4<f32>,    // xy = taille de l'image (px)
    // Torche tenue en main : xyz = position, w = portée (0 = aucune) ; couleur (linéaire).
    held_pos: vec4<f32>,
    held_color: vec4<f32>,
    rt: vec4<f32>,        // x = raytracing activé, yzw = coin bas de la grille de blocs
    // Sources de lumière proches (nombre dans light.z) : xyz = position, w = rayon de la
    // flamme (0 = bloc lumineux) ; couleur linéaire, w = portée.
    lights_pos: array<vec4<f32>, 32>,
    lights_col: array<vec4<f32>, 32>,
    // Images des sources derrière les miroirs (nombre dans light.w) : position, couleur
    // (w = portée), plan du miroir (x = axe, y = coordonnée, z = côté éclairé ±1).
    mirror_pos: array<vec4<f32>, 16>,
    mirror_col: array<vec4<f32>, 16>,
    mirror_plane: array<vec4<f32>, 16>,
    // Reflets calculés à part (voir fs_reflect) : vue-projection et position de la caméra
    // de leur dernier calcul (w = 1 si l'image des reflets est valable).
    refl_vp: mat4x4<f32>,
    refl_cam: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;
@group(0) @binding(3) var voxels: texture_3d<u32>;
@group(0) @binding(4) var palette: texture_1d<u32>;
// Dernière image des reflets : rgb = reflet, a = distance caméra → surface (0 = rien).
@group(0) @binding(5) var refl_tex: texture_2d<f32>;

const HORIZON: vec3<f32> = vec3<f32>(0.62, 0.78, 0.95);
const ZENITH: vec3<f32> = vec3<f32>(0.25, 0.47, 0.85);

fn lin(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

fn face_normal(f: u32) -> vec3<f32> {
    switch f {
        case 0u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 1u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 3u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 4u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, -1.0); }
    }
}

fn hash3(v: vec3<i32>) -> f32 {
    var h = (u32(v.x) * 73856093u) ^ (u32(v.y) * 19349663u) ^ (u32(v.z) * 83492791u);
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffu) / 65535.0;
}

fn sky_color(dir: vec3<f32>) -> vec3<f32> {
    let day = g.light.x;
    let t = clamp(dir.y, 0.0, 1.0);
    var c = mix(lin(HORIZON), lin(ZENITH), pow(t, 0.6)) * day;
    // Nuit : bleu très sombre.
    c += mix(vec3<f32>(0.010, 0.014, 0.030), vec3<f32>(0.002, 0.004, 0.012), t) * (1.0 - day);
    let s = max(dot(dir, normalize(g.sun.xyz)), 0.0);
    c += vec3<f32>(1.0, 0.9, 0.7) * (pow(s, 900.0) * 6.0 + pow(s, 12.0) * 0.12) * day;
    return c;
}

// ---------------------------------------------------------------- blocs

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) data: u32,
    @location(2) tex: u32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) block: u32,
    @location(2) @interpolate(flat) face: u32,
    @location(3) ao: f32,
    // Terrain lointain (LOD) : couleur sRGB par sommet, w = 1.
    @location(4) lod: vec4<f32>,
    // Blocs : lumière colorée des torches (0..1 par canal). Flamme : couleur émise.
    @location(5) light: vec3<f32>,
    // Couche du tableau de textures, bit 12 = émissif.
    @location(6) @interpolate(flat) tex: u32,
    // Lumière du ciel reçue (0 = grotte fermée, 1 = ciel ouvert).
    @location(7) sky: f32,
    // Reflet du bloc (0 mat … 1 miroir).
    @location(8) reflect: f32,
    // Coordonnées de texture du sommet (faces penchées, voir TEX_LOCAL_UV).
    @location(9) uv: vec2<f32>,
};

@vertex
fn vs_block(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.world = v.pos;
    o.block = 0u;
    o.sky = f32(v.data & 15u) / 15.0;
    o.reflect = f32((v.data >> 4u) & 15u) / 15.0;
    o.face = (v.data >> 8u) & 7u;
    o.ao = f32((v.data >> 11u) & 3u) / 3.0;
    o.tex = v.tex;
    o.uv = vec2<f32>(f32((v.tex >> 16u) & 255u), f32((v.tex >> 24u) & 255u)) / 128.0;
    let rgb = vec3<f32>(
        f32((v.data >> 14u) & 63u),
        f32((v.data >> 20u) & 63u),
        f32((v.data >> 26u) & 63u),
    ) / 63.0;
    if ((v.data >> 13u) & 1u) == 1u {
        o.lod = vec4<f32>(rgb, 1.0);
        o.light = vec3<f32>(0.0);
    } else {
        o.lod = vec4<f32>(0.0);
        o.light = rgb;
    }
    return o;
}

@fragment
fn fs_block_fast(i: VOut) -> @location(0) vec4<f32> {
    let c = shade_with(i, false, false);
    if c.a < 0.3 {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}

@fragment
fn fs_block(i: VOut) -> @location(0) vec4<f32> {
    let c = shade(i);
    if c.a < 0.3 {
        discard; // pixels transparents (feuillage, torche, verre clair)
    }
    return vec4<f32>(c.rgb, 1.0);
}

/// Passe translucide (eau, glace, verre teinté) : mélange selon l'alpha de la texture.
@fragment
fn fs_block_translucent(i: VOut) -> @location(0) vec4<f32> {
    let c = shade(i);
    if c.a < 0.02 {
        discard;
    }
    return vec4<f32>(c.rgb, max(c.a, 0.25));
}

// ---------------------------------------------------------------- vecteurs de mouvement (TAA)

struct MvOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
};

/// Déplacement à l'écran (pixels, y vers le bas) entre cette image et la précédente :
/// position précédente − position courante.
fn motion_of(p: vec4<f32>) -> vec2<f32> {
    let cur = g.cur_view_proj * p;
    let prev = g.prev_view_proj * p;
    let d = prev.xy / prev.w - cur.xy / cur.w;
    return d * vec2<f32>(0.5, -0.5) * g.screen.xy;
}

@fragment
fn fs_block_fast_mv(i: VOut) -> MvOut {
    let c = shade_with(i, false, false);
    if c.a < 0.3 {
        discard;
    }
    return MvOut(vec4<f32>(c.rgb, 1.0), motion_of(vec4<f32>(i.world, 1.0)));
}

@fragment
fn fs_block_mv(i: VOut) -> MvOut {
    // Reflets lus dans l'image des reflets (calculée à part, à son propre rythme).
    let c = shade_with(i, true, true);
    if c.a < 0.3 {
        discard;
    }
    return MvOut(vec4<f32>(c.rgb, 1.0), motion_of(vec4<f32>(i.world, 1.0)));
}

@fragment
fn fs_block_translucent_mv(i: VOut) -> MvOut {
    let c = shade(i);
    if c.a < 0.02 {
        discard;
    }
    return MvOut(vec4<f32>(c.rgb, max(c.a, 0.25)), vec2<f32>(0.0));
}

fn shade(i: VOut) -> vec4<f32> {
    return shade_with(i, true, false);
}

/// Reflet lu dans la dernière image des reflets (calculée à son propre rythme) : la surface
/// y est retrouvée par sa position dans le monde, donc juste même si la caméra a bougé
/// depuis. Si elle n'y figurait pas (cachée, hors champ), on lance le rayon ici.
fn cached_reflection(p: vec3<f32>, r: vec3<f32>) -> vec3<f32> {
    let clip = g.refl_vp * vec4<f32>(p, 1.0);
    let ndc = clip.xy / max(clip.w, 1e-6);
    let uv = ndc * vec2<f32>(0.5, -0.5) + 0.5;
    let dims = vec2<i32>(textureDimensions(refl_tex));
    let px = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - 1);
    let t = textureLoad(refl_tex, px, 0);
    let expected = length(p - g.refl_cam.xyz);
    let inside = clip.w > 0.0 && all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    let ok = g.refl_cam.w > 0.5 && inside && t.a > 0.0 && abs(t.a - expected) < expected * 0.02 + 0.05;
    var env = t.rgb;
    if !ok {
        env = trace(p, r);
    }
    return env;
}

/// Passe des reflets (à son propre rythme) : rayon depuis chaque surface réfléchissante
/// visible ; a = distance à la caméra pour la retrouver ensuite.
@fragment
fn fs_reflect(i: VOut) -> @location(0) vec4<f32> {
    let n = face_normal(i.face);
    let to_frag = i.world - g.cam_pos.xyz;
    let dist = length(to_frag);
    let r = reflect(to_frag / max(dist, 1e-4), n);
    let env = trace(i.world + n * 0.02, r);
    return vec4<f32>(env, dist);
}

/// `reflections` = false : blocs sans reflet (la grande majorité) ; le compilateur retire
/// tout le code des reflets, le shader est bien plus léger et la carte va plus vite.
fn shade_with(i: VOut, reflections: bool, cached: bool) -> vec4<f32> {
    let n = face_normal(i.face);
    let p = i.world - n * 0.001;            // légèrement à l'intérieur du bloc
    let cell = vec3<i32>(floor(p));
    let to_frag = i.world - g.cam_pos.xyz;
    let dist = length(to_frag);
    let fog = smoothstep(g.fog.x, g.fog.y, dist);

    // Texture du pack : coordonnées tirées de la position monde (1 bloc = 1 texture),
    // dérivées calculées avant tout branchement, sur les coordonnées continues
    // (pas de couture de mipmap aux bords des blocs, ni sur les grandes faces du LOD).
    let an = abs(n);
    let raw = select(
        select(
            select(vec2<f32>(i.world.x, -i.world.y), vec2<f32>(i.world.x, i.world.z), an.y > 0.5),
            vec2<f32>(i.world.z, -i.world.y),
            an.x > 0.5,
        ),
        i.uv,
        (i.tex & 8192u) != 0u,
    );
    let layer = i.tex & 4095u;
    // Torche (TEX_SHARP, bit 14) : toujours en pleine résolution, sinon elle s'efface de loin.
    let sharp = select(1.0, 0.0, (i.tex & 16384u) != 0u);
    let tex = textureSampleGrad(blocks, block_sampler, fract(raw), layer, dpdx(raw) * sharp, dpdy(raw) * sharp);

    // Flamme : émissive, cœur plus clair, légère ondulation.
    if (i.tex & 4096u) != 0u {
        let t = g.cam_pos.w;
        let wobble = 0.85 + 0.15 * sin(t * 11.0 + i.world.y * 20.0 + f32(cell.x * 7 + cell.z * 3));
        let core = clamp(1.0 - (fract(i.world.y) - 0.5) * 3.2, 0.0, 1.0);
        let flame = lin(i.light) * 1.3 * wobble + lin(i.light) * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
        var fc = flame;
        if fog > 0.0 {
            fc = mix(flame, sky_color(to_frag / dist), fog * 0.6);
        }
        return vec4<f32>(fc, 1.0);
    }
    // Terrain lointain : toujours opaque (les trous du feuillage disparaissent au loin).
    let alpha = select(tex.a, 1.0, i.lod.w > 0.5);
    let albedo = tex.rgb;

    // Éclairage : soleil diffus + ambiant (selon l'heure), modulé par l'occlusion ambiante.
    let sun = normalize(g.sun.xyz);
    let diffuse = max(dot(n, sun), 0.0);
    let ao = mix(0.42, 1.0, i.ao * i.ao);
    let day = g.light.x;
    // Lumière du ciel atténuée sous les toits et dans les grottes (courbe ~ quadratique,
    // comme une perception de luminosité), plus un minimum pour ne pas être tout noir.
    let sky_amount = i.sky * i.sky;
    let sky_light = ((g.sun.w + (1.0 - g.sun.w) * diffuse) * day + 0.04 * (1.0 - day)) * sky_amount + 0.012;
    // Lumière colorée des torches, avec un léger scintillement par bloc.
    let t = g.cam_pos.w;
    let flicker = 0.93 + 0.07 * sin(t * 9.0 + hash3(cell / vec3<i32>(4)) * 6.28);
    var torch = pow(i.light, vec3<f32>(2.0)) * g.light.y * flicker * mix(0.55, 1.0, i.ao);
    // Lumière de la torche tenue en main (dynamique, sans ombres) : décroît avec la distance,
    // plus forte sur les faces tournées vers le joueur.
    if g.held_pos.w > 0.0 && i.lod.w < 0.5 {
        let to_l = g.held_pos.xyz - i.world;
        let d = length(to_l);
        let f = clamp(1.0 - d / g.held_pos.w, 0.0, 1.0);
        let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
        let held = g.held_color.rgb * f * f * facing * flicker * mix(0.6, 1.0, i.ao);
        torch = max(torch, held);
    }
    // Lumière des torches renvoyée par les miroirs (raytracing).
    if g.rt.x > 0.5 && i.lod.w < 0.5 && g.light.w > 0.0 {
        torch = max(torch, mirror_light(i.world + n * 0.01, n) * flicker * mix(0.55, 1.0, i.ao));
    }
    var color = albedo * (sky_light * ao + torch);

    // Bloc visé légèrement éclairci (le contour est dessiné à part, en lignes).
    let eps = vec3<f32>(0.01);
    if g.sel_min.w > 0.5 && i.lod.w < 0.5 && all(i.world >= g.sel_min.xyz - eps) && all(i.world <= g.sel_max.xyz + eps) {
        color = color * 1.25 + 0.02;
    }

    // Reflets : miroir, métal, glace, eau. Avec le raytracing, un rayon traverse la grille de
    // blocs autour du joueur ; sans, seul le ciel se reflète.
    // Au loin, le reflet s'efface doucement, comme le brouillard de fin de vue : de 60 % à
    // 100 % de 4 chunks (≈ 38 → 64 blocs) ; au-delà la surface est opaque (et aucun rayon
    // n'est lancé : plus de FPS).
    let reflect_fade = 1.0 - smoothstep(REFLECT_RANGE * 0.6, REFLECT_RANGE, dist);
    if reflections && i.reflect > 0.0 && i.lod.w < 0.5 && reflect_fade > 0.0 {
        let v = to_frag / max(dist, 1e-4);
        let r = reflect(v, n);
        var env: vec3<f32>;
        if g.rt.x > 0.5 && cached {
            env = cached_reflection(i.world + n * 0.02, r);
        } else if g.rt.x > 0.5 {
            env = trace(i.world + n * 0.02, r);
        } else {
            env = sky_color(r);
        }
        let fresnel = pow(1.0 - max(dot(-v, n), 0.0), 5.0);
        let k = select(i.reflect * (0.55 + 0.45 * fresnel), 0.96, i.reflect > 0.99) * reflect_fade;
        color = mix(color, env, k);
        // Points lumineux : chaque source proche se reflète (couleur de sa flamme), net
        // sur un miroir, plus étalé sur le métal ; avec ou sans raytracing.
        if g.rt.x < 0.5 || i.reflect < 0.99 {
            color += specular(i.world, r, i.reflect) * flicker * reflect_fade;
        }
    }

    // Brouillard vers la couleur du ciel dans la direction du regard (calculée seulement là
    // où il y a du brouillard : les pixels proches n'en ont pas).
    if fog > 0.0 {
        color = mix(color, sky_color(to_frag / dist), fog);
    }
    return vec4<f32>(color, alpha);
}

// ---------------------------------------------------------------- ciel

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) vi: u32) -> SkyOut {
    // Triangle plein écran.
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let ndc = uv * 2.0 - 1.0;
    var o: SkyOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.ndc = ndc;
    return o;
}

fn sky_dir(ndc: vec2<f32>) -> vec3<f32> {
    let far = g.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    return normalize(far.xyz / far.w - g.cam_pos.xyz);
}

@fragment
fn fs_sky_mv(i: SkyOut) -> MvOut {
    let dir = sky_dir(i.ndc);
    // Point à l'infini : seule la rotation de la caméra le déplace.
    return MvOut(sky_shade(dir), motion_of(vec4<f32>(dir, 0.0)));
}

@fragment
fn fs_sky(i: SkyOut) -> @location(0) vec4<f32> {
    return sky_shade(sky_dir(i.ndc));
}

fn sky_shade(dir: vec3<f32>) -> vec4<f32> {
    var c = sky_color(dir);
    // Étoiles la nuit.
    let night = 1.0 - g.light.x;
    if night > 0.05 && dir.y > 0.0 {
        let q = vec3<i32>(floor(dir * 420.0));
        let h = hash3(q);
        if h > 0.9975 {
            c += vec3<f32>(0.8, 0.85, 1.0) * (h - 0.9975) * 400.0 * night * dir.y;
        }
    }
    return vec4<f32>(c, 1.0);
}

// ---------------------------------------------------------------- contour du bloc visé

/// Les 12 arêtes de la boîte visée (24 sommets, lignes), un rien plus grande que le bloc.
@vertex
fn vs_outline(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Coin : bit 0 = x, bit 1 = y, bit 2 = z. Arêtes 0-3 selon x, 4-7 selon y, 8-11 selon z.
    let edge = vi / 2u;
    let end = vi % 2u;
    var c: u32;
    if edge < 4u {
        // selon x : (y, z) fixés
        let yz = edge;
        c = select(0u, 1u, end == 1u) | ((yz & 1u) << 1u) | ((yz >> 1u) << 2u);
    } else if edge < 8u {
        let xz = edge - 4u;
        c = (xz & 1u) | (select(0u, 1u, end == 1u) << 1u) | ((xz >> 1u) << 2u);
    } else {
        let xy = edge - 8u;
        c = (xy & 1u) | ((xy >> 1u) << 1u) | (select(0u, 1u, end == 1u) << 2u);
    }
    let lo = g.sel_min.xyz - vec3<f32>(0.002);
    let hi = g.sel_max.xyz + vec3<f32>(0.002);
    let pos = vec3<f32>(
        select(lo.x, hi.x, (c & 1u) != 0u),
        select(lo.y, hi.y, (c & 2u) != 0u),
        select(lo.z, hi.z, (c & 4u) != 0u),
    );
    return g.view_proj * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_outline() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.7);
}

@fragment
fn fs_outline_mv() -> MvOut {
    return MvOut(vec4<f32>(0.0, 0.0, 0.0, 0.7), vec2<f32>(0.0));
}

// ---------------------------------------------------------------- raytracing (reflets)

const VOX: vec3<i32> = vec3<i32>(128, 64, 128);

/// Case de la grille : bits 0..11 = bloc (0 = vide), bits 12..15 = lumière du ciel
/// (0..15) ; 0xFFFF = hors de la grille.
fn voxel(c: vec3<i32>) -> u32 {
    // Sans retour anticipé (compilateur DirectX).
    let l = c - vec3<i32>(g.rt.yzw);
    let inside = all(l >= vec3<i32>(0)) && all(l < VOX);
    let raw = textureLoad(voxels, clamp(l, vec3<i32>(0), VOX - vec3<i32>(1)), 0).r;
    return select(0xffffu, raw, inside);
}

/// Reflet des sources de lumière dans la direction réfléchie `r` depuis `p`.
fn specular(p: vec3<f32>, r: vec3<f32>, k: f32) -> vec3<f32> {
    let shin = exp2(mix(5.0, 13.0, k));
    let gain = mix(0.6, 6.0, k * k);
    var c = vec3<f32>(0.0);
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let lp = g.lights_pos[j];
        let to_l = lp.xyz - p;
        let d = length(to_l);
        let reach = g.lights_col[j].w;
        let f = clamp(1.0 - d / reach, 0.0, 1.0);
        if f <= 0.0 {
            continue;
        }
        // Source étendue : on élargit le lobe selon sa taille apparente.
        let size = max(lp.w, 0.25) / max(d, 0.3);
        let cosr = max(dot(r, to_l / d), 0.0);
        let s = pow(cosr, min(shin, 2.0 / (size * size)));
        c += g.lights_col[j].rgb * s * f * f * gain;
    }
    if g.held_pos.w > 0.0 {
        let to_l = g.held_pos.xyz - p;
        let d = length(to_l);
        let f = clamp(1.0 - d / g.held_pos.w, 0.0, 1.0);
        let size = 0.1 / max(d, 0.3);
        let s = pow(max(dot(r, to_l / max(d, 1e-3)), 0.0), min(shin, 2.0 / (size * size)));
        c += g.held_color.rgb * s * f * f * gain;
    }
    return c;
}

/// Lumière colorée reçue en `p` (normale `n`) des sources proches, sans ombres, plus celle
/// que les miroirs renvoient.
fn point_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var c = vec3<f32>(0.0);
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let to_l = g.lights_pos[j].xyz - p;
        let d = length(to_l);
        c = max(c, g.lights_col[j].rgb * torch_falloff(d) * (max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.25 + 0.75));
    }
    return max(c, mirror_light(p, n));
}

/// Lumière de la torche tenue en main en `p` (même formule que sur les faces du monde) ;
/// sert aussi dans les reflets : le monde vu dans le miroir est éclairé par la torche.
/// Sans branchement (compilateur DirectX).
fn held_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let to_l = g.held_pos.xyz - p;
    let d = length(to_l);
    let f = clamp(1.0 - d / max(g.held_pos.w, 1e-3), 0.0, 1.0) * select(0.0, 1.0, g.held_pos.w > 0.0);
    let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
    return g.held_color.rgb * (f * f * facing);
}

/// Atténuation de la lumière des torches comme dans le monde : −25 % par case parcourue
/// (case par case, donc un peu plus loin qu'à vol d'oiseau), au carré dans le shader.
fn torch_falloff(d: f32) -> f32 {
    return select(0.0, pow(0.5625, d * 1.25), d < 15.0);
}

/// Palette : reflet du bloc (0 mat … 1 miroir).
fn block_reflect(id: u32) -> f32 {
    return f32((textureLoad(palette, i32(id), 0).w >> 4u) & 15u) / 15.0;
}

/// Lumière des torches renvoyée par les miroirs : chaque source a une image symétrique
/// derrière le plan d'un miroir ; elle n'éclaire `p` que si le trajet vers cette image
/// traverse vraiment une face de miroir (vérifié dans la grille de blocs).
fn mirror_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    // Sans retour anticipé ni `continue` : le compilateur DirectX (FXC) part en récursion
    // infinie sur cette fonction sinon.
    var c = vec3<f32>(0.0);
    let count = select(0u, u32(g.light.w), g.rt.x > 0.5);
    for (var j = 0u; j < count; j++) {
        let plane = g.mirror_plane[j];
        let to_l = g.mirror_pos[j].xyz - p;
        let d = length(to_l);
        let f = torch_falloff(d);
        let dir = select(select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), plane.x > 0.5 && plane.x < 1.5), vec3<f32>(1.0, 0.0, 0.0), plane.x < 0.5);
        let pa = dot(p, dir);
        let la = dot(to_l, dir);
        // `p` du côté éclairé du miroir, segment vers l'image qui croise son plan sur une
        // face de miroir (vérifiée dans la grille de blocs).
        let t = (plane.y - pa) / select(la, 1e-5, abs(la) < 1e-5);
        // Tests bon marché d'abord : les lectures de la grille (2 par image) seulement pour
        // les pixels assez proches de l'image, du bon côté du miroir. Au-delà de 10 cases la
        // lumière renvoyée est invisible (< 0,2 %).
        var ok = d < 10.0 && (pa - plane.y) * plane.z > 0.001 && t > 0.0 && t < 1.0;
        if ok {
            let hit = p + to_l * t;
            let l = vec3<i32>(floor(hit - dir * plane.z * 0.5)) - vec3<i32>(g.rt.yzw);
            let inside = all(l >= vec3<i32>(0)) && all(l < VOX);
            let id = textureLoad(voxels, clamp(l, vec3<i32>(0), VOX - vec3<i32>(1)), 0).r & 4095u;
            let refl = (textureLoad(palette, i32(id), 0).w >> 4u) & 15u;
            ok = inside && id != 0u && refl == 15u;
        }
        let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
        c = max(c, g.mirror_col[j].rgb * (f * facing * 0.9 * f32(ok)));
    }
    return c;
}

/// Torche murale : point (ou direction si `dir`) de la case vers le repère du modèle
/// droit (inverse de `wall_torch_xf` côté jeu). `code` : 1 droite, 2..5 murale.
fn torch_local(q: vec3<f32>, code: u32, dir: bool) -> vec3<f32> {
    if code == 1u {
        return q;
    }
    let k = select(1.0, 0.0, dir);
    var x = q.x;
    var z = q.z;
    if code == 2u {
        z = q.x;
        x = k - q.z;
    } else if code == 3u {
        x = q.z;
        z = k - q.x;
    } else if code == 5u {
        x = k - q.x;
        z = k - q.z;
    }
    let c = 0.9238795;
    let s = 0.3826834;
    let yp = q.y - 0.2 * k;
    let zp = z - 0.9375 * k;
    return vec3<f32>(x, yp * c - zp * s, yp * s + zp * c + 0.5 * k);
}

/// Rayon / boîte : (entrée, sortie, axe de la face d'entrée).
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

/// Flamme animée, comme celle des torches posées (`y` : hauteur dans le bloc).
fn flame_color(light: vec3<f32>, y: f32, cell: vec3<i32>) -> vec3<f32> {
    let t = g.cam_pos.w;
    let wobble = 0.85 + 0.15 * sin(t * 11.0 + y * 20.0 + f32(cell.x * 7 + cell.z * 3));
    let core = clamp(1.0 - (fract(y) - 0.5) * 3.2, 0.0, 1.0);
    return light * 1.3 * wobble + light * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
}

/// Couleur de flamme de la torche en `cell` (source la plus proche), en linéaire.
fn torch_flame(cell: vec3<i32>) -> vec3<f32> {
    let center = vec3<f32>(cell) + 0.5;
    var best = 1.5;
    var c = pow(vec3<f32>(1.0, 0.588, 0.216), vec3<f32>(2.2));
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let d = length(g.lights_pos[j].xyz - center);
        if d < best && g.lights_pos[j].w > 0.0 {
            best = d;
            c = g.lights_col[j].rgb / 1.25;
        }
    }
    return c;
}

/// Surface touchée par un rayon (l'éclairage est calculé ensuite, une seule fois).
struct RayHit {
    // Couleur de la texture, ou couleur finale si `shade` < 0 (flamme, bloc lumineux, ciel).
    color: vec3<f32>,
    t: f32,
    normal: vec3<f32>,
    // Part renvoyée plus loin (miroir), 0 sinon.
    k: f32,
    // Ombrage selon la face (< 0 : couleur déjà finale).
    shade: f32,
    // Lumière propre ajoutée (bâton d'une torche éclairé par sa flamme).
    glow: vec3<f32>,
    // Lumière du ciel reçue (0 grotte … 1 ciel ouvert), comme les faces du monde.
    sky: f32,
};

/// Un segment de rayon case par case (DDA) depuis `o` dans la direction `d`.
/// Portée des reflets (blocs) : 4 chunks. Le rayon réfléchi ne va pas plus loin (chemin
/// total, rebonds compris), avec le même fondu vers le ciel que la fin de la vue ; et les
/// surfaces plus loin que ça de la caméra ne reflètent plus.
const REFLECT_RANGE: f32 = 64.0;

fn march(o: vec3<f32>, d: vec3<f32>, max_t: f32) -> RayHit {
    var cell = vec3<i32>(floor(o));
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - o) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var axis = 1;
    var t = 0.0;
    var sky_prev = 1.0;
    // Première itération : la case de départ elle-même (torche juste devant un miroir).
    for (var i = 0; i < 97; i++) {
        if i == 0 {
            // rien : on teste la case de départ
        } else if t_max.x < t_max.y && t_max.x < t_max.z {
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
        // Au-delà de la portée des reflets : on s'arrête (ciel, fondu dans `trace`).
        if t > max_t {
            break;
        }
        let raw = voxel(cell);
        if raw == 0xffffu {
            break;
        }
        let id = raw & 4095u;
        // Ciel reçu par une face : celui de la case vide d'où arrive le rayon.
        let sky_cell = f32(raw >> 12u) / 15.0;
        let sky_face = select(sky_prev, sky_cell, i == 0);
        sky_prev = sky_cell;
        if id == 0u {
            continue;
        }
        let pal = textureLoad(palette, i32(id), 0);
        let torch = (pal.w >> 8u) & 7u;
        // Case de départ : seules les torches comptent (le rayon part de la surface).
        if i == 0 && torch == 0u {
            continue;
        }
        if torch != 0u {
            // Modèle de la torche : bâton (texture) et flamme (animée), droits ou penchés.
            let lo = torch_local(o - vec3<f32>(cell), torch, false);
            let ld = torch_local(d, torch, true);
            let hs = box_hit(lo, ld, vec3<f32>(0.4375, 0.0, 0.4375), vec3<f32>(0.5625, 0.5, 0.5625));
            let hf = box_hit(lo, ld, vec3<f32>(0.40625, 0.5, 0.40625), vec3<f32>(0.59375, 0.8125, 0.59375));
            let ok_s = hs.y >= 0.0 && hs.x >= t - 1e-4;
            let ok_f = hf.y >= 0.0 && hf.x >= t - 1e-4;
            if ok_f && (!ok_s || hf.x <= hs.x) {
                let lp = lo + ld * hf.x;
                return RayHit(flame_color(torch_flame(cell), lp.y, cell), hf.x, -d, 0.0, -1.0, vec3<f32>(0.0), 1.0);
            }
            if ok_s {
                let lp = lo + ld * hs.x;
                var uv = vec2<f32>(lp.x, lp.z);
                var layer = select(pal.z, pal.x, ld.y < 0.0);
                var shade = select(0.5, 1.0, ld.y < 0.0);
                if hs.z == 0.0 {
                    uv = vec2<f32>(lp.z, 1.0 - lp.y);
                    layer = pal.y;
                    shade = 0.62;
                } else if hs.z == 2.0 {
                    uv = vec2<f32>(lp.x, 1.0 - lp.y);
                    layer = pal.y;
                    shade = 0.8;
                }
                let albedo = textureSampleLevel(blocks, block_sampler, uv, i32(layer), 0.0).rgb;
                // Le bâton est éclairé par sa propre flamme.
                return RayHit(albedo, hs.x, -d, 0.0, shade, torch_flame(cell) * 0.9, sky_cell);
            }
            continue;
        }
        // Point touché et coordonnées de texture comme pour les faces du monde.
        let p = o + d * t;
        var uv = vec2<f32>(p.x, p.z);
        var layer = select(pal.z, pal.x, step.y < 0);
        var shade = select(0.5, 1.0, step.y < 0);
        if axis == 0 {
            uv = vec2<f32>(p.z, -p.y);
            layer = pal.y;
            shade = 0.62;
        } else if axis == 2 {
            uv = vec2<f32>(p.x, -p.y);
            layer = pal.y;
            shade = 0.8;
        }
        let fs = -vec3<f32>(step);
        let nrm = select(select(vec3<f32>(0.0, 0.0, fs.z), vec3<f32>(0.0, fs.y, 0.0), axis == 1), vec3<f32>(fs.x, 0.0, 0.0), axis == 0);
        let lod = clamp(log2(max(t, 1.0) * 0.6), 0.0, 4.0);
        let albedo = textureSampleLevel(blocks, block_sampler, fract(uv), i32(layer), lod).rgb;
        if (pal.w & 1u) != 0u {
            return RayHit(albedo * 1.2, t, nrm, 0.0, -1.0, vec3<f32>(0.0), 1.0); // bloc lumineux
        }
        let r = f32((pal.w >> 4u) & 15u) / 15.0;
        return RayHit(albedo, t, nrm, select(0.0, 0.96, r > 0.99), shade, vec3<f32>(0.0), sky_face);
    }
    return RayHit(sky_color(d), 1e9, vec3<f32>(0.0), 0.0, -1.0, vec3<f32>(0.0), 1.0);
}

/// Reflet vu depuis `o` dans la direction `d` : jusqu'à 4 rebonds de miroir en miroir.
fn trace(o0: vec3<f32>, d0: vec3<f32>) -> vec3<f32> {
    var o = o0;
    var d = d0;
    var acc = vec3<f32>(0.0);
    var thr = 1.0;
    // Miroir dans le miroir : on continue tant que le reflet apporte encore quelque chose
    // (au plus 64 rebonds ; borne lue dans les uniformes pour que le compilateur ne
    // déroule pas la boucle).
    let max_bounces = 64 + i32(g.screen.w);
    var travelled = 0.0;
    for (var bounce = 0; bounce < max_bounces; bounce++) {
        let h = march(o, d, REFLECT_RANGE - travelled);
        var color = h.color;
        if h.shade >= 0.0 {
            let p = o + d * h.t;
            // Même éclairage que les faces du monde : soleil et ciel atténués sous les toits
            // et dans les grottes, plus les torches.
            let day = g.light.x;
            let diffuse = max(dot(h.normal, normalize(g.sun.xyz)), 0.0);
            let sky_light = ((g.sun.w + (1.0 - g.sun.w) * diffuse) * day + 0.04 * (1.0 - day)) * h.sky * h.sky + 0.012;
            let lit = max(point_light(p + h.normal * 0.02, h.normal), held_light(p + h.normal * 0.02, h.normal));
            color = h.color * (sky_light * mix(1.0, h.shade, 0.35) + lit * 0.8 + h.glow * 0.5);
        }
        // Brouillard de fin de portée, comme en fin de vue : vers la couleur du ciel.
        let far = travelled + min(h.t, REFLECT_RANGE);
        color = mix(color, sky_color(d), smoothstep(REFLECT_RANGE * 0.6, REFLECT_RANGE, far));
        travelled = far;
        if h.k <= 0.0 || bounce == max_bounces - 1 || thr * h.k < 0.02 || travelled >= REFLECT_RANGE {
            acc += thr * color;
            return acc;
        }
        acc += thr * (1.0 - h.k) * color;
        thr *= h.k;
        o = o + d * h.t + h.normal * 0.002;
        d = reflect(d, h.normal);
    }
    return acc;
}
