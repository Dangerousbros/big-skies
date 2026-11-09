fn skies_frame(game: &mut crate::game::Game<'_>, elapsed: std::time::Duration) {
    crate::game::skies_glue::frame(game, elapsed);
}
