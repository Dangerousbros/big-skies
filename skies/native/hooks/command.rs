fn skies_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    if crate::game::skies_glue::command(game, command) {
        *ran = true;
    }
}
