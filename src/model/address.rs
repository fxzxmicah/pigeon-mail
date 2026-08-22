pub fn split_mailbox_list(raw: &str) -> Vec<String> {
    let mut mailboxes = Vec::new();
    let mut current = String::new();
    let mut angle_depth = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '<' if !in_quotes => {
                angle_depth += 1;
                current.push(ch);
            }
            '>' if !in_quotes => {
                angle_depth = angle_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_quotes && angle_depth == 0 => {
                push_nonempty(&mut mailboxes, &mut current);
            }
            _ => current.push(ch),
        }
    }
    push_nonempty(&mut mailboxes, &mut current);
    mailboxes
}

pub fn normalized_mailbox_address(mailbox: &str) -> String {
    let mailbox = mailbox.trim();
    let address = mailbox
        .rfind('<')
        .and_then(|start| {
            mailbox[start + 1..]
                .find('>')
                .map(|end| &mailbox[start + 1..start + 1 + end])
        })
        .unwrap_or(mailbox);
    address.trim().to_ascii_lowercase()
}

fn push_nonempty(mailboxes: &mut Vec<String>, current: &mut String) {
    let mailbox = current.trim();
    if !mailbox.is_empty() {
        mailboxes.push(mailbox.to_string());
    }
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::{normalized_mailbox_address, split_mailbox_list};

    #[test]
    fn mailbox_lists_split_only_at_top_level_commas() {
        assert_eq!(
            split_mailbox_list(
                r#""Doe, Jane" <jane@example.test>, "A \"Quoted\" Name" <quoted@example.test>, bare@example.test"#,
            ),
            [
                r#""Doe, Jane" <jane@example.test>"#,
                r#""A \"Quoted\" Name" <quoted@example.test>"#,
                "bare@example.test",
            ]
        );
    }

    #[test]
    fn mailbox_lists_ignore_empty_items_and_keep_unclosed_input_intact() {
        assert_eq!(
            split_mailbox_list(", first@example.test, , second@example.test,"),
            ["first@example.test", "second@example.test"]
        );
        assert_eq!(
            split_mailbox_list(r#""Unclosed, Name <broken@example.test>"#),
            [r#""Unclosed, Name <broken@example.test>"#]
        );
    }

    #[test]
    fn normalization_compares_display_and_bare_mailboxes_by_exact_address() {
        assert_eq!(
            normalized_mailbox_address(" Example Person <Person@Example.Test> "),
            "person@example.test"
        );
        assert_eq!(
            normalized_mailbox_address("PERSON@example.test"),
            "person@example.test"
        );
        assert_eq!(normalized_mailbox_address("  "), "");
    }
}
