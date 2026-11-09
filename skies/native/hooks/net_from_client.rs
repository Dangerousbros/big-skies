fn skies_net_from_client(game: &mut crate::game::Game<'_>, player: u128, channel: &str, data: &[u8]) {
    crate::game::skies_glue::from_client(game, player, channel, data);
}
