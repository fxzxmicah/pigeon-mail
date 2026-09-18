use std::path::Path;

pub(crate) use gettextrs::{gettext, ngettext};

const DOMAIN: &str = "pigeon";

pub(crate) fn init() -> std::io::Result<()> {
    // SAFETY: main calls this before GTK, logging, or any application threads
    // are initialized, which is the synchronization requirement of setlocale.
    unsafe {
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    }
    gettextrs::bindtextdomain(DOMAIN, Path::new(env!("PREFIX")).join("share/locale"))?;
    gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8")?;
    gettextrs::textdomain(DOMAIN)?;
    Ok(())
}

pub(crate) fn gettext_f(message: &str, arguments: &[(&str, &str)]) -> String {
    interpolate(gettext(message), arguments)
}

pub(crate) fn ngettext_f(
    singular: &str,
    plural: &str,
    count: u32,
    arguments: &[(&str, &str)],
) -> String {
    interpolate(ngettext(singular, plural, count), arguments)
}

pub(crate) fn format_datetime(timestamp_secs: i64) -> String {
    if timestamp_secs <= 0 {
        return gettext("Unknown date");
    }

    glib::DateTime::from_unix_local(timestamp_secs)
        .ok()
        .and_then(|datetime| datetime.format("%x %H:%M").ok())
        .map(|value| value.to_string())
        .unwrap_or_else(|| gettext("Unknown date"))
}

fn interpolate(message: String, arguments: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(message.len());
    let mut remaining = message.as_str();
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        let token = &remaining[start..];
        let Some(relative_end) = token.find('}') else {
            output.push_str(token);
            return output;
        };
        let end = relative_end + 1;
        let name = &token[1..relative_end];
        if let Some((_, value)) = arguments.iter().find(|(candidate, _)| *candidate == name) {
            output.push_str(value);
        } else {
            output.push_str(&token[..end]);
        }
        remaining = &token[end..];
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
mod tests {
    use super::interpolate;

    #[test]
    fn interpolation_is_single_pass_and_preserves_unknown_tokens() {
        assert_eq!(
            interpolate(
                "{sender} sent {count} messages to {recipient}".into(),
                &[("sender", "{count}"), ("count", "2")],
            ),
            "{count} sent 2 messages to {recipient}"
        );
    }
}
