use tracing_subscriber::EnvFilter;

/// Installs the global `tracing` subscriber. `RUST_LOG`, when set, overrides `filter`.
///
/// Only binaries call this; an embedding host installs its own subscriber.
pub fn init(filter: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(true).try_init();
}
