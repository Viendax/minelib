// ---------------------------------------------------------------- lumières dynamiques (absentes)
// Shaders par défaut du monde et des entités : sans le code des lumières de l'éditeur, qui
// n'est compilé (dans `dynlights.wgsl`) que la première fois qu'une lumière est posée. Un
// joueur qui ne s'en sert pas a les mêmes shaders qu'avant, et un compilateur qui
// refuserait ce code (DirectX) ne coûte que les lumières.

struct DynOut {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
};

fn dyn_active() -> bool {
    return false;
}

fn dyn_light(
    p: vec3<f32>,
    n: vec3<f32>,
    cam: vec3<f32>,
    gloss: f32,
    entity: bool,
    frag: vec2<f32>,
    size: vec2<f32>,
) -> DynOut {
    return DynOut(vec3<f32>(0.0), vec3<f32>(0.0));
}

fn dyn_light_all(p: vec3<f32>, n: vec3<f32>, cam: vec3<f32>, gloss: f32, entity: bool, frag: vec2<f32>) -> DynOut {
    return DynOut(vec3<f32>(0.0), vec3<f32>(0.0));
}
