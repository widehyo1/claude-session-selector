fn main() -> anyhow::Result<()> {
    claude_session_selector::run_from_args(std::env::args().skip(1))
}
