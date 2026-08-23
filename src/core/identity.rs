use crate::model::account::{MailAccount, SendingIdentity};
use crate::model::address::{normalized_mailbox_address, split_mailbox_list};

pub(crate) fn merge_eds_profile(
    account: &mut MailAccount,
    name: Option<&str>,
    reply_to: Option<&str>,
    aliases: Option<&str>,
) {
    let identity_name = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let identity_reply_to = reply_to
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    if let Some(primary) = account.primary_identity_mut() {
        if let Some(name) = &identity_name {
            primary.display_name = name.clone();
        }
        if let Some(reply_to) = &identity_reply_to {
            primary.reply_to = Some(reply_to.clone());
        }
    }

    let primary_identity = account.primary_identity().cloned();
    let eds_aliases = aliases.map(parse_aliases).unwrap_or_default();
    let mut merged_aliases = Vec::new();
    let mut used = vec![false; account.aliases.len()];

    for eds_alias in eds_aliases {
        let eds_display_name = eds_alias.display_name.unwrap_or_default();
        let eds_address = eds_alias.address;
        let normalized_eds_address = normalized_mailbox_address(&eds_address);
        if normalized_eds_address.is_empty() {
            continue;
        }

        let matched_index = account
            .aliases
            .iter()
            .enumerate()
            .find_map(|(index, identity)| {
                if used[index]
                    || identity.is_primary_address
                    || normalized_mailbox_address(&identity.address) != normalized_eds_address
                    || identity.display_name.trim() != eds_display_name.trim()
                {
                    return None;
                }
                Some(index)
            });

        if let Some(index) = matched_index {
            used[index] = true;
            merged_aliases.push(account.aliases[index].clone());
        } else {
            merged_aliases.push(SendingIdentity::new(
                &account.id.0,
                eds_address,
                eds_display_name,
                identity_reply_to.clone(),
                String::new(),
                String::new(),
                false,
            ));
        }
    }

    if let Some(primary) = primary_identity {
        if let Some(index) = account
            .aliases
            .iter()
            .position(|identity| identity.is_primary_address)
        {
            used[index] = true;
        }
        merged_aliases.insert(0, primary);
    }

    merged_aliases.extend(
        account
            .aliases
            .iter()
            .enumerate()
            .filter(|(index, _)| !used[*index])
            .map(|(_, identity)| identity.clone()),
    );
    account.aliases = merged_aliases;
}

struct ParsedAlias {
    address: String,
    display_name: Option<String>,
}

fn parse_aliases(raw: &str) -> Vec<ParsedAlias> {
    split_mailbox_list(raw)
        .into_iter()
        .filter_map(|item| parse_alias(&item))
        .collect()
}

fn parse_alias(item: &str) -> Option<ParsedAlias> {
    let trimmed = item.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>'))
        && start < end
    {
        let address = trimmed[start + 1..end].trim().to_string();
        if normalized_mailbox_address(&address).is_empty() {
            return None;
        }
        let display_name = trimmed[..start].trim().trim_matches('"').trim().to_string();
        return Some(ParsedAlias {
            address,
            display_name: (!display_name.is_empty()).then_some(display_name),
        });
    }

    if normalized_mailbox_address(trimmed).is_empty() {
        return None;
    }
    Some(ParsedAlias {
        address: trimmed.to_string(),
        display_name: None,
    })
}
