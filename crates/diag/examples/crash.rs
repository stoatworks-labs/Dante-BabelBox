//! Exercise the crash path end to end.
//!
//! The panic hook is process-global and runs during unwinding, which the test
//! harness owns — so this lives as an example rather than a `#[test]`. Run it
//! and read what it leaves behind:
//!
//! ```console
//! cargo run -p diag --example crash
//! ```

#[derive(serde::Serialize)]
struct DemoConfig {
    host: String,
    port: u16,
    api_token: String,
}

fn main() {
    let config = DemoConfig {
        host: "127.0.0.1".into(),
        port: 5250,
        // Should appear as `<redacted>` in the report.
        api_token: "sk-live-should-not-appear".into(),
    };

    let _guard = diag::init(
        diag::Options::new(
            "diag-crash-example",
            "DIAG_EXAMPLE",
            env!("CARGO_PKG_VERSION"),
        )
        .with_default_filter("debug")
        .with_config(&config),
    )
    .expect("starting logging");

    tracing::info!(channel = 1, layer = 10, "loaded background plate");
    tracing::debug!(command = "MIXER 1-10 FILL", "sent to server");
    tracing::warn!(retries = 3, "server slow to answer");

    let channels: Vec<u16> = vec![1, 2, 3];
    // A plausible fault rather than an artificial one: an off-by-one into a
    // channel table is exactly the kind of thing this has to explain.
    let requested = 7;
    tracing::info!(requested, "looking up channel");
    println!("channel {} is {}", requested, channels[requested]);
}
