// Minelib — bras à la première personne, éclairé comme le modèle du joueur (F5) : sa
// couleur (skin, ombrage selon l'orientation de la face dans le monde) × la lumière du lieu.
// Sommets déjà projetés (champ de vision de la main), avec leur position dans le monde
// (devant les yeux) et leur normale pour les lumières dynamiques (viewmodel_arm_lit.wgsl).

// Début des uniformes du monde.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct ArmIn {
    @location(0) clip: vec4<f32>,
    @location(1) world: vec3<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) albedo: vec3<f32>,
    @location(4) light: vec3<f32>,
};

struct ArmOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) normal: vec3<f32>,
    @location(2) @interpolate(flat) albedo: vec3<f32>,
    @location(3) @interpolate(flat) light: vec3<f32>,
};

@vertex
fn vs_arm(v: ArmIn) -> ArmOut {
    var o: ArmOut;
    o.clip = v.clip;
    o.world = v.world;
    o.normal = v.normal;
    o.albedo = v.albedo;
    o.light = v.light;
    return o;
}

// Sans lumières dynamiques (aucune posée, ou leurs shaders pas encore prêts).
@fragment
fn fs_arm(i: ArmOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.albedo * i.light, 1.0);
}
