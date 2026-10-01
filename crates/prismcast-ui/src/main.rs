//! The `prismcast` binary: boots tracing, the core actor on a background
//! Tokio runtime, and the Relm4 application.

use prismcast_ui::app::AppModel;
use prismcast_ui::bridge::CoreBridge;
use relm4::RelmApp;
use tracing::error;
use tracing_subscriber::EnvFilter;

fn main() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,prismcast=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let (bridge, core_thread) = match CoreBridge::spawn_background() {
        Ok(pair) => pair,
        Err(error) => {
            error!(%error, "failed to start the core runtime");
            std::process::exit(1);
        }
    };

    let app = RelmApp::new("io.github.worxbend.prismcast");
    app.run_async::<AppModel>(bridge);

    // The window-close flow shut the actor down gracefully; the runtime
    // thread exits once the actor is gone.
    if core_thread.join().is_err() {
        error!("core thread panicked");
        std::process::exit(1);
    }
}
