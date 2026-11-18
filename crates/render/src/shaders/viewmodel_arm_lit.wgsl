// Bras à la première personne avec les lumières dynamiques : le même calcul que pour le
// modèle du joueur en F5 (entity.wgsl puis dyn_deferred.wgsl : entité, brillance 0,15,
// reflets à moitié), en sa position devant les yeux, avec toutes les lumières (le bras n'est
// pas à sa place dans les tuiles de l'image du monde) et sans les ombres des boîtes des
// joueurs (celles du corps, posé autrement qu'à la première personne). Ajouté à la suite de
// viewmodel_arm.wgsl ; gbuffer.wgsl et dynlights.wgsl suivent.

@fragment
fn fs_arm_lit(i: ArmOut) -> @location(0) vec4<f32> {
    let n = normalize(i.normal);
    let d = dyn_light_all(i.world, n, g.cam_pos.xyz, 0.15, true, i.clip.xy);
    // Poids des reflets d'une entité, tel que l'image des surfaces le garde.
    let spec = gb_unpack_spec(gb_pack_spec(0.5, false)).x;
    return vec4<f32>(i.albedo * i.light + i.albedo * d.diffuse + d.specular * spec, 1.0);
}
