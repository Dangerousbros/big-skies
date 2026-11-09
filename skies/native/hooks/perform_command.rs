fn skies_perform_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::skies_glue::perform_command(game, command, ran);
}
