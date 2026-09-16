pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter()));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(debug_assertions)]
fn default_filter() -> &'static str {
    // Keep dependencies at info while enabling every Pigeon diagnostic compiled into a
    // development build, including the development-only provider/FFI probes.
    "info,pigeon=debug"
}

#[cfg(not(debug_assertions))]
fn default_filter() -> &'static str {
    "info"
}

pub fn report_failure(operation: &'static str, error: &anyhow::Error) {
    let category = error_category(error);
    tracing::error!(operation, category, "operation failed");
    report_development_detail(operation, error);
}

pub fn report_deferred(operation: &'static str, error: &anyhow::Error) {
    let category = error_category(error);
    tracing::warn!(operation, category, "operation deferred");
    report_development_detail(operation, error);
}

pub fn report_invalid_input(operation: &'static str, error: &impl std::fmt::Display) {
    tracing::warn!(operation, category = "input", "request rejected");
    report_development_detail(operation, error);
}

#[cfg(debug_assertions)]
fn report_development_detail(operation: &'static str, error: &impl std::fmt::Display) {
    tracing::debug!(target: "pigeon::development", operation, error = %format_args!("{error:#}"), "development failure detail");
}

#[cfg(not(debug_assertions))]
fn report_development_detail(_: &'static str, _: &impl std::fmt::Display) {}

fn error_category(error: &anyhow::Error) -> &'static str {
    match crate::failure::classify_failure(error) {
        crate::model::event::RefreshFailureKind::Connectivity => "connectivity",
        crate::model::event::RefreshFailureKind::Authentication => "authentication",
        crate::model::event::RefreshFailureKind::Storage => "storage",
        crate::model::event::RefreshFailureKind::Backend => "backend",
    }
}

#[cfg(test)]
mod tests {
    use super::{default_filter, error_category};

    #[cfg(debug_assertions)]
    #[test]
    fn development_build_enables_project_debug_logging_by_default() {
        assert_eq!(default_filter(), "info,pigeon=debug");
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_build_keeps_info_as_the_default_log_level() {
        assert_eq!(default_filter(), "info");
    }

    #[test]
    fn log_categories_do_not_echo_sensitive_error_text() {
        assert_eq!(
            error_category(&anyhow::anyhow!("TLS failure for private host")),
            "connectivity"
        );
        assert_eq!(
            error_category(&anyhow::anyhow!("unclassified private detail")),
            "backend"
        );
        assert_eq!(
            error_category(&anyhow::Error::new(glib::Error::new(
                gio::IOErrorEnum::NoSpace,
                "opaque",
            ))),
            "storage"
        );
    }
}
