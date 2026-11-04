//! The piloted Dbento binary: the gpui-pilot host runs the app first thing.

fn main() -> std::process::ExitCode {
    gpui_pilot_host::run(dbento_pilot::Dbento)
}
