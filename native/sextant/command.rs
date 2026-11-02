fn intercept_command(
    game: &mut crate::game::Game<'_>,
    command: &str,
    ran: &mut bool,
) {
    crate::game::sextant::command(game, command, ran);
}
