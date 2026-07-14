fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "--session-replay-benchmark")
    {
        println!(
            "{}",
            nopal_desktop_spike::bench::run_session_replay().to_json()
        );
        return;
    }
    if arguments
        .iter()
        .any(|argument| argument == "--terminal-benchmark")
    {
        println!("{}", nopal_desktop_spike::bench::run().to_json());
        return;
    }
    if let Err(error) = nopal_desktop_spike::native_entry::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
