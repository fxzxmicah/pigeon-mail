use crate::model::event::RefreshFailureKind;

pub fn classify_failure(error: &anyhow::Error) -> RefreshFailureKind {
    for cause in error.chain() {
        let Some(error) = cause.downcast_ref::<glib::Error>() else {
            continue;
        };
        if let Some(kind) = error.kind::<gio::IOErrorEnum>() {
            return match kind {
                gio::IOErrorEnum::ProxyAuthFailed | gio::IOErrorEnum::ProxyNeedAuth => {
                    RefreshFailureKind::Authentication
                }
                gio::IOErrorEnum::TimedOut
                | gio::IOErrorEnum::HostNotFound
                | gio::IOErrorEnum::HostUnreachable
                | gio::IOErrorEnum::NetworkUnreachable
                | gio::IOErrorEnum::ConnectionRefused
                | gio::IOErrorEnum::ProxyFailed
                | gio::IOErrorEnum::NotConnected
                | gio::IOErrorEnum::BrokenPipe => RefreshFailureKind::Connectivity,
                gio::IOErrorEnum::NoSpace
                | gio::IOErrorEnum::ReadOnly
                | gio::IOErrorEnum::TooManyOpenFiles => RefreshFailureKind::Storage,
                _ => RefreshFailureKind::Backend,
            };
        }
        if let Some(kind) = error.kind::<gio::DBusError>() {
            return match kind {
                gio::DBusError::AuthFailed => RefreshFailureKind::Authentication,
                gio::DBusError::NoReply
                | gio::DBusError::IoError
                | gio::DBusError::NoServer
                | gio::DBusError::Timeout
                | gio::DBusError::NoNetwork
                | gio::DBusError::Disconnected
                | gio::DBusError::TimedOut => RefreshFailureKind::Connectivity,
                _ => RefreshFailureKind::Backend,
            };
        }
    }
    classify_failure_message(error)
}

pub(crate) fn classify_failure_message(error: &impl std::fmt::Display) -> RefreshFailureKind {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("unauthorized")
        || contains_any(
            &message,
            &["authentication failed", "auth failed", "sign in required"],
        )
        || (message.contains("credential")
            && contains_any(&message, &["rejected", "expired", "invalid"]))
    {
        RefreshFailureKind::Authentication
    } else if contains_any(
        &message,
        &[
            "tls",
            "network",
            "connect",
            "offline",
            "unreachable",
            "timeout",
        ],
    ) {
        RefreshFailureKind::Connectivity
    } else if contains_any(
        &message,
        &[
            "no space",
            "disk full",
            "read-only",
            "maildir",
            "local cache",
        ],
    ) {
        RefreshFailureKind::Storage
    } else {
        RefreshFailureKind::Backend
    }
}

fn contains_any(message: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| message.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::classify_failure;
    use crate::model::event::RefreshFailureKind;

    #[test]
    fn message_fallback_distinguishes_safe_failure_categories() {
        assert_eq!(
            classify_failure(&anyhow::anyhow!("TLS connection failed")),
            RefreshFailureKind::Connectivity
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("OAuth credentials were rejected")),
            RefreshFailureKind::Authentication
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("Maildir is read-only")),
            RefreshFailureKind::Storage
        );
        assert_eq!(
            classify_failure(&anyhow::anyhow!("unexpected provider response")),
            RefreshFailureKind::Backend
        );
    }

    #[test]
    fn explicit_authentication_failure_wins_over_connection_context() {
        assert_eq!(
            classify_failure(&anyhow::anyhow!("authentication failed while connecting")),
            RefreshFailureKind::Authentication
        );
    }

    #[test]
    fn configured_auth_method_does_not_hide_a_tls_failure() {
        assert_eq!(
            classify_failure(&anyhow::anyhow!(
                "connect failed (backend='example', auth='XOAUTH2'): TLS handshake terminated"
            )),
            RefreshFailureKind::Connectivity
        );
    }

    #[test]
    fn typed_glib_errors_do_not_depend_on_localized_messages() {
        let offline = anyhow::Error::new(glib::Error::new(
            gio::IOErrorEnum::NetworkUnreachable,
            "opaque",
        ));
        let storage = anyhow::Error::new(glib::Error::new(gio::IOErrorEnum::NoSpace, "opaque"));
        let authentication =
            anyhow::Error::new(glib::Error::new(gio::DBusError::AuthFailed, "opaque"));

        assert_eq!(classify_failure(&offline), RefreshFailureKind::Connectivity);
        assert_eq!(classify_failure(&storage), RefreshFailureKind::Storage);
        assert_eq!(
            classify_failure(&authentication),
            RefreshFailureKind::Authentication
        );
    }
}
