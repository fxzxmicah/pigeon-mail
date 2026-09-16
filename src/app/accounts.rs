use crate::integration::backend::{SharedMailBackendRouter, mail_backend_router};
use crate::integration::settings::SettingsStore;
use crate::integration::stub::stub_account;
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::mail::MailboxMode;
use crate::model::settings::AppSettings;

pub(crate) struct RediscoveryInput {
    settings: AppSettings,
    current_account_id: Option<MailAccountId>,
    accounts: Vec<MailAccount>,
}

impl RediscoveryInput {
    pub(crate) fn new(
        settings: AppSettings,
        current_account_id: Option<MailAccountId>,
        accounts: Vec<MailAccount>,
    ) -> Self {
        Self {
            settings,
            current_account_id,
            accounts,
        }
    }
}

pub(crate) enum AccountRediscovery {
    Unchanged,
    Catalog {
        accounts: Vec<MailAccount>,
        changed_routes: Vec<MailAccountId>,
    },
    Replacement { seed: MailboxSeed },
}

pub(crate) struct MailboxSeed {
    pub(crate) accounts: Vec<MailAccount>,
    pub(crate) settings: AppSettings,
    pub(crate) mode: MailboxMode,
}

pub(crate) enum AccountBootstrap {
    Ready(MailboxSeed),
    Unavailable {
        seed: MailboxSeed,
        error: anyhow::Error,
    },
}

#[derive(Clone)]
pub struct AccountRuntime {
    settings_store: SettingsStore,
    backend_router: SharedMailBackendRouter,
}

impl AccountRuntime {
    pub fn new() -> Self {
        Self {
            settings_store: SettingsStore::new(),
            backend_router: mail_backend_router(),
        }
    }

    pub fn initial_catalog(&self) -> AccountBootstrap {
        let settings = self.settings_store.load();
        match self.seed_for_settings(settings.clone()) {
            Ok(seed) => AccountBootstrap::Ready(seed),
            Err(error) => AccountBootstrap::Unavailable {
                seed: Self::unavailable_seed(settings),
                error,
            },
        }
    }

    fn unavailable_seed(settings: AppSettings) -> MailboxSeed {
        MailboxSeed {
            accounts: vec![stub_account()],
            settings,
            mode: MailboxMode::Unavailable,
        }
    }

    fn seed_for_settings(&self, settings: AppSettings) -> anyhow::Result<MailboxSeed> {
        let (accounts, bindings) = crate::integration::account::discover()?.into_parts();
        Ok(self.seed_from_accounts(settings, accounts, bindings))
    }

    pub(crate) fn apply_discovered_catalog(
        &self,
        input: RediscoveryInput,
        catalog: crate::integration::account::AccountCatalog,
    ) -> AccountRediscovery {
        let (accounts, bindings) = catalog.into_parts();
        let changed_routes = self.backend_router.update_binding_catalog(&bindings);
        self.reconcile_catalog(input, accounts, changed_routes)
    }

    fn reconcile_catalog(
        &self,
        input: RediscoveryInput,
        accounts: Vec<MailAccount>,
        changed_routes: Vec<MailAccountId>,
    ) -> AccountRediscovery {
        if input
            .current_account_id
            .as_ref()
            .is_some_and(|current| accounts.iter().any(|account| account.id == *current))
        {
            if changed_routes.is_empty() && input.accounts == accounts {
                return AccountRediscovery::Unchanged;
            }
            return AccountRediscovery::Catalog {
                accounts,
                changed_routes,
            };
        }
        let vanished_index = input
            .current_account_id
            .as_ref()
            .filter(|current| !crate::integration::stub::is_stub_account_id(current))
            .and_then(|current| {
                input.accounts.iter().position(|account| account.id == *current)
            });
        let mut settings = input.settings;
        if let Some(vanished_index) = vanished_index {
            settings.selected_account_id = accounts
                .get(vanished_index.min(accounts.len().saturating_sub(1)))
                .or_else(|| accounts.first())
                .map(|account| account.id.clone());
        }
        AccountRediscovery::Replacement {
            seed: self.seed_from_installed_accounts(settings, accounts),
        }
    }

    fn seed_from_accounts(
        &self,
        settings: AppSettings,
        accounts: Vec<MailAccount>,
        bindings: Vec<crate::integration::account::EdsAccountBinding>,
    ) -> MailboxSeed {
        self.backend_router.update_binding_catalog(&bindings);
        self.seed_from_installed_accounts(settings, accounts)
    }

    fn seed_from_installed_accounts(
        &self,
        mut settings: AppSettings,
        accounts: Vec<MailAccount>,
    ) -> MailboxSeed {
        let selected_account_id = settings
            .selected_account_id
            .as_ref()
            .and_then(|selected| accounts.iter().find(|account| account.id == *selected))
            .or_else(|| accounts.first())
            .map(|account| account.id.clone());
        settings.selected_account_id = selected_account_id.clone();
        let accounts = if selected_account_id.is_some() {
            accounts
        } else {
            vec![stub_account()]
        };
        MailboxSeed {
            accounts,
            settings,
            mode: MailboxMode::Loading,
        }
    }

    pub(crate) fn backend_router(&self) -> SharedMailBackendRouter {
        self.backend_router.clone()
    }

    pub fn save_preferences(&self, settings: &AppSettings) {
        if let Err(error) = self.settings_store.save_preferences(settings) {
            crate::logging::report_failure("preferences-save", &error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::account::{MailAccountId, SendingIdentity};

    fn account(id: &str) -> MailAccount {
        MailAccount::new(
            MailAccountId(id.into()),
            id.into(),
            SendingIdentity::new(
                "owner@example.com".into(),
                "Owner".into(),
                None,
                crate::model::account::Signature::default(),
            ),
        )
    }

    #[test]
    fn discovery_failure_retains_the_real_selected_account_preference() {
        let unavailable = AccountRuntime::unavailable_seed(AppSettings {
            selected_account_id: Some(MailAccountId("account-1".into())),
            prefer_html_view: true,
        });

        assert_eq!(
            unavailable
                .settings
                .selected_account_id
                .as_ref()
                .map(|account_id| account_id.0.as_str()),
            Some("account-1")
        );
        assert!(unavailable.settings.prefer_html_view);
        assert_eq!(unavailable.mode, MailboxMode::Unavailable);
    }

    #[test]
    fn rediscovery_preserves_any_presented_real_account_that_still_exists() {
        let runtime = AccountRuntime::new();
        let accounts = vec![account("account-1"), account("account-2")];
        let input = RediscoveryInput::new(
            AppSettings {
                selected_account_id: Some(MailAccountId("account-2".into())),
                ..AppSettings::default()
            },
            Some(MailAccountId("account-2".into())),
            accounts.clone(),
        );

        assert!(matches!(
            runtime.reconcile_catalog(input, accounts, Vec::new()),
            AccountRediscovery::Unchanged
        ));
    }

    #[test]
    fn rediscovery_replaces_a_disappeared_account_by_catalog_position() {
        let runtime = AccountRuntime::new();
        let old_accounts = vec![account("account-1"), account("account-2")];
        let new_accounts = vec![account("account-1"), account("account-3")];
        let input = RediscoveryInput::new(
            AppSettings {
                selected_account_id: Some(MailAccountId("account-2".into())),
                ..AppSettings::default()
            },
            Some(MailAccountId("account-2".into())),
            old_accounts,
        );

        let AccountRediscovery::Replacement { seed } =
            runtime.reconcile_catalog(input, new_accounts, Vec::new())
        else {
            panic!("a disappeared presented account must replace the mailbox");
        };
        assert_eq!(
            seed.settings.selected_account_id,
            Some(MailAccountId("account-3".into()))
        );
    }

    #[test]
    fn disappeared_presented_account_uses_its_position_when_the_preference_is_stale() {
        let runtime = AccountRuntime::new();
        let input = RediscoveryInput::new(
            AppSettings {
                selected_account_id: Some(MailAccountId("account-1".into())),
                ..AppSettings::default()
            },
            Some(MailAccountId("account-2".into())),
            vec![
                account("account-1"),
                account("account-2"),
                account("account-3"),
            ],
        );

        let AccountRediscovery::Replacement { seed } = runtime.reconcile_catalog(
            input,
            vec![account("account-1"), account("account-3")],
            Vec::new(),
        ) else {
            panic!("the vanished presented account must select its positional replacement");
        };
        assert_eq!(
            seed.settings.selected_account_id,
            Some(MailAccountId("account-3".into()))
        );
    }

    #[test]
    fn rediscovery_from_stub_restores_the_retained_real_account_preference() {
        let runtime = AccountRuntime::new();
        let preferred = MailAccountId("account-2".into());
        let input = RediscoveryInput::new(
            AppSettings {
                selected_account_id: Some(preferred.clone()),
                ..AppSettings::default()
            },
            Some(crate::integration::stub::stub_account_id()),
            vec![stub_account()],
        );

        let AccountRediscovery::Replacement { seed } = runtime.reconcile_catalog(
            input,
            vec![account("account-1"), account("account-2")],
            Vec::new(),
        ) else {
            panic!("a recovered EDS catalog must replace the stub mailbox");
        };
        assert_eq!(seed.settings.selected_account_id, Some(preferred));
    }
}
