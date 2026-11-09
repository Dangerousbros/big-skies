fn skies_shader_params(game: &crate::game::Game<'_>, params: &mut [[f32; 4]; 8]) {
    crate::game::skies_glue::shader_params(game, params);
}
