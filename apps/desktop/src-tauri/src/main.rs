fn main() {
    sigil_desktop_app::clear_startup_failure();
    match std::panic::catch_unwind(sigil_desktop_app::run) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("Sigil desktop failed to start: {error}");
            sigil_desktop_app::record_startup_failure(error.as_ref());
            std::process::exit(1);
        }
        Err(payload) => {
            sigil_desktop_app::record_startup_panic(payload.as_ref());
            std::process::exit(1);
        }
    }
}
