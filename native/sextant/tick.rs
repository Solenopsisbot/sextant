fn advance(game: &mut crate::game::Game<'_>) {
    crate::game::sextant::tick(game);
}
