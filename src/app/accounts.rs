use crate::integration::backend::{
    SharedMailBackend, lazy_mail_backend, stub_backend, sync_account_identity_to_eds,
};
use crate::integration::settings::SettingsStore;
use crate::integration::stub::stub_account;
use crate::model::account::{AliasId, MailAccount, SendingIdentity};
use crate::model::address::normalized_mailbox_address;
use crate::model::settings::{AccountProfile, AliasProfile, AppSettings};
use crate::ui::mailbox::MailboxViewModel;

#[derive(Clone)]
pub struct AccountRuntime {
    settings_store: SettingsStore,
}

impl AccountRuntime {
    pub fn new() -> Self {
        Self {
            settings_store: SettingsStore::new(),
        }
    }

    pub fn initial_mailbox(&self) -> MailboxViewModel {
        let settings = self.settings_store.load().unwrap_or_else(|error| {
            crate::logging::report_failure("settings-load", &error);
            AppSettings::default()
        });

        self.mailbox_for_settings(settings)
    }

    pub(crate) fn unavailable_mailbox() -> MailboxViewModel {
        MailboxViewModel::from_snapshot(
            vec![stub_account()],
            AppSettings::default(),
            stub_backend(),
            crate::model::event::AccountMailboxSnapshot {
                mode: crate::model::mail::MailboxMode::StubUnavailable,
                folders: Vec::new(),
                conversations: Vec::new(),
            },
        )
    }

    pub(crate) fn mailbox_for_settings(&self, settings: AppSettings) -> MailboxViewModel {
        let (accounts, bindings, discovery_failed) =
            match crate::integration::backend::discover_accounts() {
                Ok(catalog) => {
                    let (accounts, bindings) = catalog.into_parts();
                    (accounts, bindings, false)
                }
                Err(error) => {
                    crate::logging::report_failure("eds-account-discovery", &error);
                    (Vec::<MailAccount>::new(), Vec::new(), true)
                }
            };
        self.mailbox_from_accounts(settings, accounts, bindings, discovery_failed, None, &[])
    }

    pub(crate) fn rediscover_mailbox(
        &self,
        settings: AppSettings,
        backend: Option<SharedMailBackend>,
        changed_accounts: &[crate::model::account::MailAccountId],
    ) -> anyhow::Result<MailboxViewModel> {
        let (accounts, bindings) = crate::integration::backend::discover_accounts()?.into_parts();
        Ok(self.mailbox_from_accounts(
            settings,
            accounts,
            bindings,
            false,
            backend,
            changed_accounts,
        ))
    }

    fn mailbox_from_accounts(
        &self,
        settings: AppSettings,
        accounts: Vec<MailAccount>,
        bindings: Vec<crate::integration::backend::EdsAccountBinding>,
        discovery_failed: bool,
        existing_backend: Option<SharedMailBackend>,
        invalidated_accounts: &[crate::model::account::MailAccountId],
    ) -> MailboxViewModel {
        let settings = normalize_settings_for_accounts(settings, &accounts);
        let accounts = apply_account_profiles(accounts, &settings);
        let selected_account_id = settings
            .selected_account_id
            .as_deref()
            .and_then(|selected| accounts.iter().find(|account| account.id.0 == selected))
            .or_else(|| accounts.first())
            .map(|account| account.id.clone());
        if let Some(backend) = existing_backend.as_ref() {
            backend.update_binding_catalog(&bindings, invalidated_accounts);
        }
        let (backend, mailbox_mode) = if let Some(account_id) = selected_account_id.as_ref() {
            let backend = existing_backend.unwrap_or_else(|| {
                let backend = lazy_mail_backend();
                backend.update_binding_catalog(&bindings, invalidated_accounts);
                backend
            });
            let mode = futures::executor::block_on(backend.activate_account(account_id))
                .unwrap_or_else(|error| {
                    crate::logging::report_failure("mail-account-activation", &error);
                    crate::model::mail::MailboxMode::StubUnavailable
                });
            (backend, mode)
        } else {
            (
                stub_backend(),
                if discovery_failed {
                    crate::model::mail::MailboxMode::StubUnavailable
                } else {
                    crate::model::mail::MailboxMode::StubNoAccount
                },
            )
        };
        let accounts = if accounts.is_empty() {
            vec![stub_account()]
        } else {
            accounts
        };
        let initial_account_id = selected_account_id.unwrap_or_else(|| accounts[0].id.clone());
        let (snapshot, cache_failed) =
            load_initial_cached_mail(&initial_account_id, &backend, mailbox_mode);
        let mut mailbox = MailboxViewModel::from_snapshot(accounts, settings, backend, snapshot);
        if cache_failed && let Some(account_id) = mailbox.current_account_id() {
            mailbox
                .set_refresh_failure(account_id, crate::model::event::RefreshFailureKind::Storage);
        }
        mailbox
    }

    pub(crate) fn settings_for_mailbox(&self, mailbox: &MailboxViewModel) -> AppSettings {
        settings_for_mailbox(mailbox)
    }

    pub fn save_mailbox(&self, mailbox: &MailboxViewModel) {
        let settings = settings_for_mailbox(mailbox);

        if let Err(error) = self.settings_store.save(&settings) {
            crate::logging::report_failure("settings-save", &error);
            return;
        }
        if let Some(account) = mailbox.current_account()
            && let Some(binding) = mailbox.eds_binding(&account.id)
            && let Err(error) = sync_account_identity_to_eds(&binding, account)
        {
            crate::logging::report_deferred("eds-identity-writeback", &error);
        }
    }
}

fn load_initial_cached_mail(
    account_id: &crate::model::account::MailAccountId,
    backend: &SharedMailBackend,
    mode: crate::model::mail::MailboxMode,
) -> (crate::model::event::AccountMailboxSnapshot, bool) {
    let mail_service = crate::core::mail::MailService::new(backend.clone());
    match futures::executor::block_on(async {
        let folders = mail_service.list_folders(account_id).await?;
        let conversations = if let Some(folder) = folders.first() {
            mail_service
                .list_conversations(
                    account_id,
                    &folder.id,
                    0,
                    crate::model::mail::CONVERSATION_PAGE_SIZE,
                )
                .await?
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>(crate::model::event::AccountMailboxSnapshot {
            mode,
            folders,
            conversations,
        })
    }) {
        Ok(snapshot) => (snapshot, false),
        Err(error) => {
            crate::logging::report_failure("initial-mail-cache-load", &error);
            (
                crate::model::event::AccountMailboxSnapshot {
                    mode,
                    folders: Vec::new(),
                    conversations: Vec::new(),
                },
                true,
            )
        }
    }
}

fn settings_for_mailbox(mailbox: &MailboxViewModel) -> AppSettings {
    AppSettings {
        selected_account_id: mailbox
            .current_account()
            .map(|account| account.id.0.clone()),
        prefer_html_view: mailbox.prefer_html_view(),
        account_profiles: collect_account_profiles(mailbox),
    }
}

fn apply_account_profiles(
    mut accounts: Vec<MailAccount>,
    settings: &AppSettings,
) -> Vec<MailAccount> {
    for account in &mut accounts {
        let Some(profile) = settings
            .account_profiles
            .iter()
            .find(|profile| profile.account_id == account.id.0)
        else {
            continue;
        };

        if !profile.account_name.trim().is_empty() {
            account.display_name = profile.account_name.clone();
        }

        let primary = PrimaryIdentityContext::from_account(account);
        let aliases = account.aliases.clone();
        let mut alias_claims = vec![None; aliases.len()];
        let mut claimed_profiles = vec![false; profile.aliases.len()];

        for (profile_index, alias) in profile.aliases.iter().enumerate() {
            if let Some(alias_index) = aliases.iter().enumerate().find_map(|(index, identity)| {
                (alias_claims[index].is_none() && identity.id.0 == alias.alias_id).then_some(index)
            }) {
                alias_claims[alias_index] = Some(profile_index);
                claimed_profiles[profile_index] = true;
            }
        }

        for (profile_index, alias) in profile.aliases.iter().enumerate() {
            if claimed_profiles[profile_index] {
                continue;
            }
            let profile_is_primary = alias.alias_id == primary.alias_id.0;
            if let Some(alias_index) = aliases.iter().enumerate().find_map(|(index, identity)| {
                if alias_claims[index].is_some()
                    || identity.is_primary_address != profile_is_primary
                    || normalized_mailbox_address(&identity.address)
                        != normalized_mailbox_address(&alias.address)
                    || identity.display_name.trim() != alias.username.trim()
                {
                    return None;
                }
                Some(index)
            }) {
                alias_claims[alias_index] = Some(profile_index);
                claimed_profiles[profile_index] = true;
            }
        }

        let mut merged_aliases = Vec::new();
        for (mut identity, claim) in aliases.into_iter().zip(alias_claims) {
            if let Some(profile_index) = claim {
                apply_profile_to_identity(&mut identity, &profile.aliases[profile_index], &primary);
                merged_aliases.push(identity);
                continue;
            }

            let has_local_semantic_match = profile.aliases.iter().any(|alias| {
                identity.is_primary_address == (alias.alias_id == primary.alias_id.0)
                    && normalized_mailbox_address(&identity.address)
                        == normalized_mailbox_address(&alias.address)
                    && identity.display_name.trim() == alias.username.trim()
            });
            if identity.is_primary_address || !has_local_semantic_match {
                merged_aliases.push(identity);
            }
        }

        for (profile_index, alias) in profile.aliases.iter().enumerate() {
            if claimed_profiles[profile_index] {
                continue;
            }
            let mut identity = SendingIdentity::with_id(
                AliasId(alias.alias_id.clone()),
                alias.address.clone(),
                alias.username.clone(),
                None,
                String::new(),
                String::new(),
                alias.is_default,
                alias.alias_id == primary.alias_id.0,
            );
            apply_profile_to_identity(&mut identity, alias, &primary);
            merged_aliases.push(identity);
        }

        if !merged_aliases
            .iter()
            .any(|identity| identity.is_primary_address)
        {
            merged_aliases.insert(
                0,
                primary.into_identity(merged_aliases.is_empty(), account.display_name.clone()),
            );
        }

        if !merged_aliases.iter().any(|identity| identity.is_default) {
            if let Some(primary) = merged_aliases
                .iter_mut()
                .find(|identity| identity.is_primary_address)
            {
                primary.is_default = true;
            } else if let Some(first) = merged_aliases.first_mut() {
                first.is_default = true;
            }
        }

        account.aliases = merged_aliases;
    }

    accounts
}

fn collect_account_profiles(mailbox: &MailboxViewModel) -> Vec<AccountProfile> {
    mailbox
        .accounts
        .iter()
        .map(|account| AccountProfile {
            account_id: account.id.0.clone(),
            account_name: account.display_name.clone(),
            aliases: account
                .aliases
                .iter()
                .map(|identity| AliasProfile {
                    alias_id: identity.id.0.clone(),
                    username: identity.display_name.clone(),
                    address: identity.address.clone(),
                    reply_to: identity.reply_to.clone(),
                    signature_text: identity.signature_text.clone(),
                    is_default: identity.is_default,
                })
                .collect(),
        })
        .collect()
}

fn normalize_settings_for_accounts(settings: AppSettings, accounts: &[MailAccount]) -> AppSettings {
    let AppSettings {
        selected_account_id,
        prefer_html_view,
        account_profiles,
    } = settings;

    let account_ids = accounts
        .iter()
        .map(|account| account.id.0.as_str())
        .collect::<Vec<_>>();

    let selected_account_id = selected_account_id.filter(|selected| {
        account_ids
            .iter()
            .any(|account_id| account_id == &selected.as_str())
    });

    let account_profiles = account_profiles
        .into_iter()
        .filter(|profile| {
            account_ids
                .iter()
                .any(|account_id| account_id == &profile.account_id.as_str())
        })
        .collect();

    AppSettings {
        selected_account_id,
        prefer_html_view,
        account_profiles,
    }
}

fn apply_profile_to_identity(
    identity: &mut SendingIdentity,
    profile: &AliasProfile,
    primary: &PrimaryIdentityContext,
) {
    let is_primary = profile.alias_id == primary.alias_id.0;
    identity.id = AliasId(profile.alias_id.clone());
    identity.address = if is_primary {
        primary.address.clone()
    } else {
        profile.address.clone()
    };
    identity.display_name = profile.username.clone();
    identity.reply_to = profile
        .reply_to
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            if is_primary {
                primary.reply_to.clone()
            } else {
                None
            }
        });
    identity.signature_html = glib::markup_escape_text(&profile.signature_text)
        .to_string()
        .replace('\n', "<br>");
    identity.signature_text = profile.signature_text.clone();
    identity.is_default = profile.is_default;
    identity.is_primary_address = is_primary;
}

struct PrimaryIdentityContext {
    alias_id: AliasId,
    address: String,
    display_name: String,
    reply_to: Option<String>,
}

impl PrimaryIdentityContext {
    fn from_account(account: &MailAccount) -> Self {
        let primary = account
            .primary_or_first_identity()
            .expect("a discovered mail account must have a sending identity");

        Self {
            alias_id: primary.id.clone(),
            address: primary.address.clone(),
            display_name: primary.display_name.clone(),
            reply_to: primary.reply_to.clone(),
        }
    }

    fn into_identity(self, is_default: bool, signature_name: String) -> SendingIdentity {
        SendingIdentity::with_id(
            self.alias_id,
            self.address,
            self.display_name,
            self.reply_to,
            format!("<p>{}</p>", signature_name),
            signature_name,
            is_default,
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::account::MailAccountId;

    fn identity(
        id: &str,
        address: &str,
        username: &str,
        is_default: bool,
        is_primary: bool,
    ) -> SendingIdentity {
        SendingIdentity::with_id(
            AliasId(id.to_string()),
            address.to_string(),
            username.to_string(),
            None,
            String::new(),
            String::new(),
            is_default,
            is_primary,
        )
    }

    fn profile(id: &str, address: &str, username: &str) -> AliasProfile {
        AliasProfile {
            alias_id: id.to_string(),
            username: username.to_string(),
            address: address.to_string(),
            reply_to: None,
            signature_text: String::new(),
            is_default: false,
        }
    }

    fn merge_profiles(
        mut aliases: Vec<SendingIdentity>,
        profiles: Vec<AliasProfile>,
    ) -> Vec<SendingIdentity> {
        let account_id = "account-1";
        aliases.insert(
            0,
            identity(
                "account-1:primary",
                "primary@example.com",
                "Primary",
                true,
                true,
            ),
        );
        let accounts = vec![MailAccount {
            id: MailAccountId(account_id.to_string()),
            display_name: "Account".to_string(),
            aliases,
        }];
        let mut local_profiles = vec![profile(
            "account-1:primary",
            "primary@example.com",
            "Primary",
        )];
        local_profiles[0].is_default = true;
        local_profiles.extend(profiles);
        let settings = AppSettings {
            selected_account_id: Some(account_id.to_string()),
            prefer_html_view: true,
            account_profiles: vec![AccountProfile {
                account_id: account_id.to_string(),
                account_name: "Account".to_string(),
                aliases: local_profiles,
            }],
        };

        apply_account_profiles(accounts, &settings)
            .into_iter()
            .next()
            .expect("test account should remain present")
            .aliases
    }

    #[test]
    fn mailbox_html_preference_is_persisted_in_both_directions() {
        for prefer_html_view in [false, true] {
            let account = MailAccount {
                id: MailAccountId("account-fixture-1".into()),
                display_name: "Example Account".into(),
                aliases: vec![identity(
                    "account-fixture-1:primary",
                    "owner@example.com",
                    "Example Owner",
                    true,
                    true,
                )],
            };
            let mut mailbox = MailboxViewModel::from_snapshot(
                vec![account],
                AppSettings::default(),
                crate::integration::backend::stub_backend(),
                crate::model::event::AccountMailboxSnapshot {
                    mode: crate::model::mail::MailboxMode::StubUnavailable,
                    folders: Vec::new(),
                    conversations: Vec::new(),
                },
            );
            mailbox.set_prefer_html_view(prefer_html_view);
            let persisted = settings_for_mailbox(&mailbox);

            assert_eq!(
                persisted.selected_account_id.as_deref(),
                Some("account-fixture-1")
            );
            assert_eq!(persisted.prefer_html_view, prefer_html_view);
            assert_eq!(persisted.account_profiles.len(), 1);
            assert_eq!(
                persisted.account_profiles[0].aliases[0].alias_id,
                "account-fixture-1:primary"
            );
        }
    }

    #[test]
    fn registry_catalog_hydrates_identity_without_activating_the_account() {
        let untouched = MailAccount {
            id: MailAccountId("account-fixture-1".into()),
            display_name: "First Account".into(),
            aliases: vec![identity(
                "account-fixture-1:primary",
                "first@example.com",
                "First User",
                true,
                true,
            )],
        };
        let hydrated = MailAccount {
            id: MailAccountId("account-fixture-2".into()),
            display_name: "Second Account".into(),
            aliases: vec![identity(
                "account-fixture-2:primary",
                "second@example.com",
                "Second User",
                true,
                true,
            )],
        };
        let mut accounts = vec![untouched, hydrated];
        crate::core::identity::merge_eds_profile(
            &mut accounts[1],
            Some("Provider User"),
            Some("provider-reply@example.net"),
            Some("Provider Alias <alias@example.net>"),
        );

        assert_eq!(accounts[0].aliases.len(), 1);
        let primary = accounts[1].primary_identity().unwrap();
        assert_eq!(primary.display_name, "Provider User");
        assert_eq!(
            primary.reply_to.as_deref(),
            Some("provider-reply@example.net")
        );
        assert_eq!(
            accounts[1]
                .aliases
                .iter()
                .filter(|alias| alias.address == "alias@example.net")
                .count(),
            1
        );
    }

    #[test]
    fn local_alias_ids_claim_matching_eds_aliases_once() {
        let account_id = "account-fixture-1";
        let primary = SendingIdentity::with_id(
            AliasId(format!("{account_id}:primary")),
            "owner@example.com".to_string(),
            "Example Owner".to_string(),
            None,
            String::new(),
            String::new(),
            true,
            true,
        );
        let mut accounts = vec![MailAccount {
            id: MailAccountId(account_id.to_string()),
            display_name: "Example Account".to_string(),
            aliases: vec![primary],
        }];
        crate::core::identity::merge_eds_profile(
            &mut accounts[0],
            Some("Example Owner"),
            None,
            Some("Example Owner <owner@example.com>, Project Alias <alias@example.net>"),
        );
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "selected_account_id": "account-fixture-1",
                "prefer_html_view": true,
                "account_profiles": [{
                    "account_id": "account-fixture-1",
                    "account_name": "Example Account",
                    "aliases": [
                        {
                            "alias_id": "account-fixture-1:primary",
                            "username": "Example Owner",
                            "address": "owner@example.com",
                            "reply_to": null,
                            "signature_text": "Primary signature",
                            "is_default": true
                        },
                        {
                            "alias_id": "account-fixture-1:alias1",
                            "username": "Example Owner",
                            "address": "owner@example.com",
                            "reply_to": "alias1-reply@example.com",
                            "signature_text": "Alias one signature",
                            "is_default": false
                        },
                        {
                            "alias_id": "account-fixture-1:alias2",
                            "username": "Project Alias",
                            "address": "alias@example.net",
                            "reply_to": "alias2-reply@example.com",
                            "signature_text": "Alias two signature",
                            "is_default": false
                        }
                    ]
                }]
            }"#,
        )
        .expect("test settings JSON should deserialize");

        let accounts = apply_account_profiles(accounts, &settings);
        let aliases = &accounts[0].aliases;

        assert_eq!(aliases.len(), 3);
        assert_eq!(
            aliases
                .iter()
                .map(|identity| identity.id.0.as_str())
                .collect::<Vec<_>>(),
            vec![
                "account-fixture-1:primary",
                "account-fixture-1:alias1",
                "account-fixture-1:alias2",
            ]
        );
        assert_eq!(
            aliases
                .iter()
                .filter(|identity| identity.id.0 == format!("{account_id}:alias2"))
                .count(),
            1
        );
        let alias2 = aliases
            .iter()
            .find(|identity| identity.address == "alias@example.net")
            .expect("alias2 should be present");
        assert_eq!(alias2.id.0, "account-fixture-1:alias2");
        assert_eq!(alias2.reply_to.as_deref(), Some("alias2-reply@example.com"));
        assert_eq!(alias2.signature_text, "Alias two signature");
    }

    #[test]
    fn semantic_matching_normalizes_email_case_and_surrounding_whitespace() {
        let aliases = merge_profiles(
            vec![identity(
                "account-1:eds:alias",
                "  ALIAS@Example.COM ",
                " Alias User ",
                false,
                false,
            )],
            vec![profile(
                "account-1:local-alias",
                "alias@example.com",
                "Alias User",
            )],
        );

        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[1].id.0, "account-1:local-alias");
        assert_eq!(aliases[1].address, "alias@example.com");
        assert_eq!(aliases[1].display_name, "Alias User");
    }

    #[test]
    fn same_address_with_different_usernames_remains_distinct() {
        let aliases = merge_profiles(
            vec![
                identity(
                    "account-1:eds:second",
                    "same@example.com",
                    "Second",
                    false,
                    false,
                ),
                identity(
                    "account-1:eds:first",
                    "same@example.com",
                    "First",
                    false,
                    false,
                ),
            ],
            vec![
                profile("account-1:first", "same@example.com", "First"),
                profile("account-1:second", "same@example.com", "Second"),
            ],
        );

        assert_eq!(aliases.len(), 3);
        assert_eq!(aliases[1].id.0, "account-1:second");
        assert_eq!(aliases[2].id.0, "account-1:first");
    }

    #[test]
    fn one_eds_alias_can_only_be_claimed_by_one_local_profile() {
        let aliases = merge_profiles(
            vec![identity(
                "account-1:eds:shared",
                "shared@example.com",
                "Shared",
                false,
                false,
            )],
            vec![
                profile("account-1:first", "shared@example.com", "Shared"),
                profile("account-1:second", "shared@example.com", "Shared"),
            ],
        );

        assert_eq!(aliases.len(), 3);
        assert_eq!(aliases[1].id.0, "account-1:first");
        assert_eq!(aliases[2].id.0, "account-1:second");
    }

    #[test]
    fn unmatched_local_and_eds_aliases_are_both_preserved() {
        let aliases = merge_profiles(
            vec![identity(
                "account-1:eds:only",
                "eds@example.com",
                "EDS Only",
                false,
                false,
            )],
            vec![profile(
                "account-1:local-only",
                "local@example.com",
                "Local Only",
            )],
        );

        assert_eq!(aliases.len(), 3);
        assert_eq!(aliases[1].id.0, "account-1:eds:only");
        assert_eq!(aliases[2].id.0, "account-1:local-only");
    }

    #[test]
    fn exact_id_match_suppresses_a_duplicate_eds_semantic_alias() {
        let aliases = merge_profiles(
            vec![
                identity(
                    "account-1:stable",
                    "duplicate@example.com",
                    "Duplicate",
                    false,
                    false,
                ),
                identity(
                    "account-1:eds:duplicate",
                    "duplicate@example.com",
                    "Duplicate",
                    false,
                    false,
                ),
            ],
            vec![profile(
                "account-1:stable",
                "duplicate@example.com",
                "Duplicate",
            )],
        );

        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[1].id.0, "account-1:stable");
    }
}
