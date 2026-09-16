use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};

#[derive(Clone)]
struct SettingsAccountDraft {
    account: MailAccount,
    display_name: String,
}

impl SettingsAccountDraft {
    fn new(account: MailAccount) -> Self {
        let display_name = account.display_name.clone();
        Self {
            account,
            display_name,
        }
    }

    fn finish(&self) -> Option<MailAccount> {
        let mut account = self.account.clone();
        account.rename(self.display_name.clone()).then_some(account)
    }

    fn rebase_onto(&self, baseline: &MailAccount, incoming: MailAccount) -> Self {
        let mut rebased = Self::new(incoming);
        if self.display_name != baseline.display_name {
            rebased.display_name = self.display_name.clone();
        }
        if self.account == *baseline {
            return rebased;
        }

        for old in baseline.aliases() {
            if self.account.identity(&old.address).is_some() {
                continue;
            }
            if rebased.account.is_default_identity(&old.address) {
                let primary = rebased.account.primary_identity().address.clone();
                rebased.account.set_default_identity(&primary);
            }
            rebased.account.remove_identity(&old.address);
        }

        for identity in self.account.aliases() {
            match baseline.identity(&identity.address) {
                Some(old)
                    if old != identity
                        && rebased.account.identity(&identity.address).is_some() =>
                {
                    rebased.account.update_identity(
                        &identity.address,
                        identity.display_name.clone(),
                        identity.address.clone(),
                        identity.reply_to.clone(),
                        identity.signature.clone(),
                    );
                }
                None => {
                    rebased.account.add_identity(identity.clone());
                }
                _ => {}
            }
        }

        if self.account.default_identity().address != baseline.default_identity().address {
            rebased
                .account
                .set_default_identity(&self.account.default_identity().address);
        }
        rebased
    }
}

#[derive(Clone)]
pub(super) struct AliasEditorTarget {
    account_id: MailAccountId,
    original_address: Option<String>,
}

pub(super) struct SettingsViewSnapshot {
    pub(super) account_names: Vec<String>,
    pub(super) selected_account: usize,
    pub(super) account: Option<MailAccount>,
    pub(super) selected_address: Option<String>,
}

pub(super) struct SettingsDraftState {
    baseline: Vec<MailAccount>,
    drafts: Vec<SettingsAccountDraft>,
    selected_account: usize,
    selected_address: Option<String>,
    editor: Option<AliasEditorTarget>,
}

impl SettingsDraftState {
    pub(super) fn new(accounts: Vec<MailAccount>, selected_account: usize) -> Self {
        let selected_account = selected_account.min(accounts.len().saturating_sub(1));
        let mut state = Self {
            baseline: accounts.clone(),
            drafts: accounts
                .into_iter()
                .map(SettingsAccountDraft::new)
                .collect(),
            selected_account,
            selected_address: None,
            editor: None,
        };
        state.normalize_selection();
        state
    }

    fn selected_draft(&self) -> Option<&SettingsAccountDraft> {
        self.drafts.get(self.selected_account)
    }

    fn selected_draft_mut(&mut self) -> Option<&mut SettingsAccountDraft> {
        self.drafts.get_mut(self.selected_account)
    }

    pub(super) fn editor_target(&self) -> Option<AliasEditorTarget> {
        self.editor.clone()
    }

    pub(super) fn view_snapshot(&self) -> SettingsViewSnapshot {
        SettingsViewSnapshot {
            account_names: self
                .drafts
                .iter()
                .map(|draft| draft.display_name.clone())
                .collect(),
            selected_account: self.selected_account,
            account: self
                .selected_draft()
                .map(|draft| draft.account.clone()),
            selected_address: self.selected_address.clone(),
        }
    }

    fn normalize_selection(&mut self) {
        let selected = self.selected_address.as_deref().and_then(|address| {
            self.selected_draft()
                .and_then(|draft| draft.account.identity(address))
                .map(|identity| identity.address.clone())
        });
        self.selected_address = selected.or_else(|| {
            self.selected_draft()
                .map(|draft| draft.account.default_identity().address.clone())
        });
    }

    pub(super) fn select_account(&mut self, index: usize) -> bool {
        if index >= self.drafts.len() || self.selected_account == index {
            return false;
        }
        self.selected_account = index;
        self.selected_address = None;
        self.normalize_selection();
        true
    }

    pub(super) fn select_identity(&mut self, index: Option<usize>) {
        self.selected_address = index.and_then(|index| {
            self.selected_draft()
                .and_then(|draft| draft.account.aliases().get(index))
                .map(|identity| identity.address.clone())
        });
    }

    pub(super) fn rename_selected(&mut self, display_name: String) -> Option<usize> {
        let index = self.selected_account;
        self.selected_draft_mut()?.display_name = display_name;
        Some(index)
    }

    pub(super) fn begin_edit(&mut self) -> Option<(MailAccount, SendingIdentity)> {
        let address = self.selected_address.as_deref()?;
        let draft = self.selected_draft()?;
        let account = draft.account.clone();
        let identity = account.identity(address)?.clone();
        self.editor = Some(AliasEditorTarget {
            account_id: account.id.clone(),
            original_address: Some(identity.address.clone()),
        });
        Some((account, identity))
    }

    pub(super) fn begin_add(&mut self) -> bool {
        let Some(account_id) = self
            .selected_draft()
            .map(|draft| draft.account.id.clone())
        else {
            return false;
        };
        self.editor = Some(AliasEditorTarget {
            account_id,
            original_address: None,
        });
        true
    }

    pub(super) fn finish_editor(&mut self) {
        self.editor = None;
    }

    pub(super) fn accounts_to_save(&self) -> Option<Vec<MailAccount>> {
        self.drafts
            .iter()
            .map(SettingsAccountDraft::finish)
            .collect()
    }

    pub(super) fn record_saved(&mut self, accounts: Vec<MailAccount>) {
        for account in accounts {
            if let Some(draft) = self
                .drafts
                .iter_mut()
                .find(|draft| draft.account.id == account.id)
                && draft.finish().as_ref() == Some(&account)
            {
                *draft = SettingsAccountDraft::new(account.clone());
            }
            match self
                .baseline
                .iter()
                .position(|baseline| baseline.id == account.id)
            {
                Some(index) => self.baseline[index] = account,
                None => self.baseline.push(account),
            }
        }
    }

    pub(super) fn apply_editor(
        &mut self,
        target: &AliasEditorTarget,
        identity: SendingIdentity,
    ) -> bool {
        let Some(index) = self
            .drafts
            .iter()
            .position(|draft| draft.account.id == target.account_id)
        else {
            return false;
        };
        let address = identity.address.clone();
        let account = &mut self.drafts[index].account;
        let changed = match target.original_address.as_deref() {
            Some(original) => account.update_identity(
                original,
                identity.display_name,
                identity.address,
                identity.reply_to,
                identity.signature,
            ),
            None => account.add_identity(identity),
        };
        if !changed {
            return false;
        }
        self.selected_account = index;
        self.selected_address = Some(address);
        true
    }

    pub(super) fn remove_selected_identity(&mut self) -> bool {
        let Some(address) = self.selected_address.clone() else {
            return false;
        };
        let Some(draft) = self.drafts.get_mut(self.selected_account) else {
            return false;
        };
        if !draft.account.remove_identity(&address) {
            return false;
        }
        self.selected_address = None;
        self.normalize_selection();
        true
    }

    pub(super) fn set_selected_default(&mut self) -> bool {
        let Some(address) = self.selected_address.clone() else {
            return false;
        };
        let Some(draft) = self.drafts.get_mut(self.selected_account) else {
            return false;
        };
        draft.account.set_default_identity(&address)
    }

    pub(super) fn rebase(
        &mut self,
        accounts: Vec<MailAccount>,
    ) -> Option<(MailAccount, SendingIdentity)> {
        let previous_editor_identity = self.editor.as_ref().and_then(|editor| {
            let draft = self
                .drafts
                .iter()
                .find(|draft| draft.account.id == editor.account_id)?;
            let identity = draft.account.identity(editor.original_address.as_deref()?)?;
            Some((
                draft.account.id.clone(),
                draft.account.is_primary_identity(&identity.address),
                identity.clone(),
            ))
        });
        let old_index = self.selected_account;
        let old_id = self
            .drafts
            .get(old_index)
            .map(|draft| draft.account.id.clone());
        let retained = old_id
            .and_then(|id| accounts.iter().position(|account| account.id == id));
        if retained.is_none() {
            self.selected_address = None;
        }
        self.selected_account = retained
            .unwrap_or_else(|| old_index.min(accounts.len().saturating_sub(1)));
        self.drafts = rebase_settings_drafts(&self.baseline, &self.drafts, &accounts);
        self.baseline = accounts;
        self.normalize_selection();

        let editor = self.editor.clone()?;
        let target = self
            .drafts
            .iter()
            .find(|draft| draft.account.id == editor.account_id);
        let editor_identity = match (target, editor.original_address.as_deref()) {
            (Some(_), None) => return None,
            (Some(draft), Some(address)) => {
                let identity = draft
                    .account
                    .identity(address)
                    .cloned()
                    .unwrap_or_else(|| draft.account.default_identity().clone());
                Some((draft.account.clone(), identity))
            }
            (None, _) => self.selected_draft().map(|draft| {
                (
                    draft.account.clone(),
                    draft.account.default_identity().clone(),
                )
            }),
        };
        if let Some((account, identity)) = editor_identity.as_ref() {
            self.editor = Some(AliasEditorTarget {
                account_id: account.id.clone(),
                original_address: Some(identity.address.clone()),
            });
        }
        editor_identity.filter(|(account, identity)| {
            previous_editor_identity.as_ref().is_none_or(|(account_id, primary, previous)| {
                account_id != &account.id
                    || *primary != account.is_primary_identity(&identity.address)
                    || previous != identity
            })
        })
    }
}

fn rebase_settings_drafts(
    baseline: &[MailAccount],
    drafts: &[SettingsAccountDraft],
    incoming: &[MailAccount],
) -> Vec<SettingsAccountDraft> {
    incoming
        .iter()
        .cloned()
        .map(|account| {
            let stored = baseline.iter().find(|stored| stored.id == account.id);
            let draft = drafts.iter().find(|draft| draft.account.id == account.id);
            match (stored, draft) {
                (Some(stored), Some(draft)) => draft.rebase_onto(stored, account),
                _ => SettingsAccountDraft::new(account),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> MailAccount {
        let mut account = MailAccount::new(
            MailAccountId("account".into()),
            "Account".into(),
            SendingIdentity::new(
                "owner@example.com".into(),
                "Owner".into(),
                None,
                Default::default(),
            ),
        );
        assert!(account.add_identity(SendingIdentity::new(
            "alias@example.com".into(),
            "Alias".into(),
            None,
            Default::default(),
        )));
        account
    }

    #[test]
    fn registry_changes_rebase_unrelated_settings_edits() {
        let baseline = account();
        let mut draft = SettingsAccountDraft::new(baseline.clone());
        assert!(draft.account.update_identity(
            "alias@example.com",
            "Local alias".into(),
            "alias@example.com".into(),
            None,
            Default::default(),
        ));
        let mut incoming = baseline.clone();
        assert!(incoming.rename("Remote name".into()));

        let rebased = rebase_settings_drafts(&[baseline], &[draft], &[incoming]);

        assert_eq!(rebased[0].account.display_name, "Remote name");
        assert_eq!(
            rebased[0]
                .account
                .identity("alias@example.com")
                .unwrap()
                .display_name,
            "Local alias"
        );
    }

    #[test]
    fn registry_changes_preserve_only_locally_edited_account_names() {
        let baseline = account();
        let mut incoming = baseline.clone();
        assert!(incoming.rename("Remote name".into()));
        let untouched = SettingsAccountDraft::new(baseline.clone());
        let mut edited = untouched.clone();
        edited.display_name = "Local name".into();

        let untouched = rebase_settings_drafts(
            std::slice::from_ref(&baseline),
            &[untouched],
            std::slice::from_ref(&incoming),
        );
        let edited = rebase_settings_drafts(
            std::slice::from_ref(&baseline),
            &[edited],
            std::slice::from_ref(&incoming),
        );

        assert_eq!(untouched[0].display_name, "Remote name");
        assert_eq!(edited[0].display_name, "Local name");
    }

    #[test]
    fn edits_after_a_successful_save_rebase_against_the_submitted_snapshot() {
        let mut state = SettingsDraftState::new(vec![account()], 0);
        state.rename_selected("  Saved name  ".into()).unwrap();
        let submitted = state.accounts_to_save().unwrap();
        state.record_saved(submitted.clone());

        assert_eq!(
            state.view_snapshot().account_names,
            vec!["Saved name".to_string()]
        );

        state.rename_selected("New edit".into()).unwrap();
        state.rebase(submitted);

        assert_eq!(
            state.view_snapshot().account_names,
            vec!["New edit".to_string()]
        );
    }

    #[test]
    fn removed_accounts_are_not_retained_by_settings() {
        let account = account();
        assert!(rebase_settings_drafts(
            std::slice::from_ref(&account),
            &[SettingsAccountDraft::new(account.clone())],
            &[],
        )
        .is_empty());
    }

    #[test]
    fn alias_mutations_keep_selection_owned_by_the_settings_state() {
        let mut state = SettingsDraftState::new(vec![account()], 0);
        state.select_identity(Some(1));
        let (_, identity) = state.begin_edit().expect("the selected alias exists");
        let target = state.editor.clone().expect("editing records its target");

        assert!(state.apply_editor(
            &target,
            SendingIdentity::new(
                "renamed@example.com".into(),
                identity.display_name,
                identity.reply_to,
                identity.signature,
            ),
        ));
        assert_eq!(
            state.selected_address.as_deref(),
            Some("renamed@example.com")
        );

        assert!(state.remove_selected_identity());
        assert_eq!(state.selected_address.as_deref(), Some("owner@example.com"));
    }

    #[test]
    fn invalid_account_selection_cannot_replace_the_settings_state() {
        let mut state = SettingsDraftState::new(vec![account()], 0);
        let before = state.view_snapshot();

        assert!(!state.select_account(usize::MAX));
        assert!(!state.select_account(0));

        let after = state.view_snapshot();
        assert_eq!(after.selected_account, before.selected_account);
        assert_eq!(after.selected_address, before.selected_address);
        assert_eq!(after.account.unwrap(), before.account.unwrap());
    }

    #[test]
    fn registry_rebase_keeps_an_in_progress_alias_addition_for_a_retained_account() {
        let account = account();
        let mut state = SettingsDraftState::new(vec![account.clone()], 0);
        state.editor = Some(AliasEditorTarget {
            account_id: account.id.clone(),
            original_address: None,
        });
        let mut incoming = account;
        assert!(incoming.rename("Updated account".into()));

        assert!(state.rebase(vec![incoming]).is_none());
        assert!(matches!(
            state.editor,
            Some(AliasEditorTarget {
                original_address: None,
                ..
            })
        ));
    }

    #[test]
    fn registry_rebase_retargets_an_open_editor_when_its_account_disappears() {
        let removed = account();
        let replacement = MailAccount::new(
            MailAccountId("replacement".into()),
            "Replacement".into(),
            SendingIdentity::new(
                "replacement@example.net".into(),
                "Replacement".into(),
                None,
                Default::default(),
            ),
        );
        let mut state = SettingsDraftState::new(vec![removed.clone()], 0);
        state.editor = Some(AliasEditorTarget {
            account_id: removed.id,
            original_address: Some("alias@example.com".into()),
        });

        let (account, identity) = state
            .rebase(vec![replacement.clone()])
            .expect("the editor should move to the replacement account");

        assert_eq!(account.id, replacement.id);
        assert_eq!(identity.address, "replacement@example.net");
        assert_eq!(state.editor.unwrap().account_id, replacement.id);
    }

    #[test]
    fn registry_rebase_reloads_an_open_editor_only_when_its_identity_changes() {
        let mut incoming = account();
        let mut state = SettingsDraftState::new(vec![incoming.clone()], 0);
        state.select_identity(Some(1));
        state.begin_edit().unwrap();

        assert!(state.rebase(vec![incoming.clone()]).is_none());
        assert!(incoming.rename("Renamed account".into()));
        assert!(incoming.update_identity(
            "owner@example.com", "Updated owner".into(), "owner@example.com".into(),
            None, Default::default(),
        ));
        assert!(state.rebase(vec![incoming.clone()]).is_none());

        assert!(incoming.update_identity(
            "alias@example.com", "Updated alias".into(), "alias@example.com".into(),
            None, Default::default(),
        ));
        let (_, identity) = state.rebase(vec![incoming.clone()]).unwrap();
        assert_eq!(identity, incoming.identity("alias@example.com").unwrap().clone());
        assert!(state.rebase(vec![incoming]).is_none());
        assert_eq!(state.editor.unwrap().original_address.as_deref(), Some("alias@example.com"));
    }
}
