use percent_encoding::percent_decode_str;

use crate::model::address::{normalized_mailbox_address, split_mailbox_list};
use crate::model::mail::MailtoRequest;

pub fn parse(uri: &str) -> anyhow::Result<MailtoRequest> {
    let (scheme, remainder) = uri
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("URI has no scheme"))?;
    anyhow::ensure!(scheme.eq_ignore_ascii_case("mailto"), "URI is not mailto");
    let remainder = remainder
        .split_once('#')
        .map_or(remainder, |(value, _)| value);
    let (path, query) = remainder
        .split_once('?')
        .map_or((remainder, None), |(path, query)| (path, Some(query)));

    let mut request = MailtoRequest::default();
    let path = decode_component(path)?;
    append_recipients(&mut request.to, &path);

    for field in query.into_iter().flat_map(|query| query.split('&')) {
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        let name = decode_component(name)?;
        let value = decode_component(value)?;
        match name.to_ascii_lowercase().as_str() {
            "to" => append_recipients(&mut request.to, &value),
            "cc" => append_recipients(&mut request.cc, &value),
            "bcc" => append_recipients(&mut request.bcc, &value),
            "subject" if request.subject.is_empty() => {
                request.subject = single_line(&value);
            }
            "body" if request.body.is_empty() => request.body = value,
            _ => {}
        }
    }

    Ok(request)
}

fn decode_component(value: &str) -> anyhow::Result<String> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            anyhow::ensure!(
                index + 2 < bytes.len()
                    && bytes[index + 1].is_ascii_hexdigit()
                    && bytes[index + 2].is_ascii_hexdigit(),
                "URI contains an invalid percent escape"
            );
            index += 3;
        } else {
            index += 1;
        }
    }

    Ok(percent_decode_str(value).decode_utf8()?.into_owned())
}

fn append_recipients(target: &mut Vec<String>, value: &str) {
    for recipient in split_mailbox_list(value) {
        if recipient.contains(['\r', '\n']) {
            continue;
        }
        let normalized = normalized_mailbox_address(&recipient);
        if normalized.is_empty() {
            continue;
        }
        if !target
            .iter()
            .any(|existing| normalized_mailbox_address(existing) == normalized)
        {
            target.push(recipient);
        }
    }
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_path_query_recipients_and_encoded_content() {
        let request = parse(
            "mailto:alice@example.com?to=bob%40example.net&cc=copy%40example.org&bcc=hidden%40example.com&subject=Hello%20world&body=Line%201%0ALine%202",
        )
        .unwrap();

        assert_eq!(request.to, ["alice@example.com", "bob@example.net"]);
        assert_eq!(request.cc, ["copy@example.org"]);
        assert_eq!(request.bcc, ["hidden@example.com"]);
        assert_eq!(request.subject, "Hello world");
        assert_eq!(request.body, "Line 1\nLine 2");
    }

    #[test]
    fn empty_mailto_opens_a_blank_composer() {
        assert_eq!(parse("mailto:").unwrap(), Default::default());
    }

    #[test]
    fn deduplicates_recipients_and_rejects_header_line_breaks() {
        let request = parse(
            "mailto:person@example.com,Example%20Person%20%3CPERSON%40example.com%3E?cc=ok%40example.net%2Cbad%0ABcc%3Ahidden%40example.com&subject=Hello%0ABcc%3Ahidden%40example.com",
        )
        .unwrap();

        assert_eq!(request.to, ["person@example.com"]);
        assert_eq!(request.cc, ["ok@example.net"]);
        assert_eq!(request.subject, "Hello Bcc:hidden@example.com");
    }

    #[test]
    fn preserves_a_quoted_display_name_containing_a_comma() {
        let request =
            parse("mailto:%22Doe%2C%20Jane%22%20%3Cjane%40example.test%3E,other%40example.test")
                .unwrap();

        assert_eq!(
            request.to,
            ["\"Doe, Jane\" <jane@example.test>", "other@example.test",]
        );
    }

    #[test]
    fn trims_empty_recipients_and_uses_the_first_subject_and_body() {
        let request = parse(
            "mailto:,first@example.com,,?TO=second%40example.net&subject=First%20subject&SUBJECT=ignored&body=First%20body&BODY=ignored",
        )
        .unwrap();

        assert_eq!(request.to, ["first@example.com", "second@example.net"]);
        assert_eq!(request.subject, "First subject");
        assert_eq!(request.body, "First body");
    }

    #[test]
    fn preserves_path_plus_tags_and_decodes_escaped_query_delimiters() {
        let request = parse(
            "mailto:person+tag@example.com?subject=C+++notes&body=A%26B%3FC%23D#ignored-fragment",
        )
        .unwrap();

        assert_eq!(request.to, ["person+tag@example.com"]);
        assert_eq!(request.subject, "C+++notes");
        assert_eq!(request.body, "A&B?C#D");
    }

    #[test]
    fn rejects_unrelated_and_malformed_uris() {
        assert!(parse("https://example.com/").is_err());
        assert!(parse("not a uri").is_err());
        assert!(parse("mailto:%FF").is_err());
        assert!(parse("mailto:person@example.com?subject=bad%escape").is_err());
    }
}
