use crate::core::mail::MailService;
use crate::integration::backend::SharedMailBackend;
use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};
use crate::model::event::{
    AccountMailboxSnapshot, MailboxContentSnapshot, MessageAction, RefreshFailureKind,
};
use crate::model::mail::{
    CONVERSATION_PAGE_SIZE, ConversationId, ConversationSummary, FolderId, MailFolder, MailboxMode,
    MessageDetail,
};
use crate::model::settings::AppSettings;
use gtk::glib;

pub struct MailboxViewModel {
    service: MailService,
    pub mailbox_mode: MailboxMode,
    pub accounts: Vec<MailAccount>,
    pub selected_account: usize,
    pub folders: Vec<MailFolder>,
    pub selected_folder: usize,
    pub threads: Vec<ConversationSummary>,
    pub has_more_threads: bool,
    thread_page_offset: usize,
    pending_thread_page: Option<ThreadPageRequest>,
    pending_search: Option<SearchRequest>,
    pending_account_activation: Option<AccountActivationRequest>,
    pending_mailbox_reload: Option<MailboxReloadRequest>,
    pending_message_actions: std::collections::HashSet<(MailAccountId, ConversationId)>,
    refresh_failures: std::collections::HashMap<MailAccountId, RefreshFailureKind>,
    pub selected_thread: Option<ConversationId>,
    pub message_detail: Option<MessageDetail>,
    pub message_detail_loading: bool,
    pending_message_detail: Option<MessageDetailRequest>,
    request_ids: RequestSequence,
    pub message_detail_error: Option<String>,
    pub search_query: String,
    pub search_error: Option<String>,
    prefer_html_view: bool,
}

#[derive(Default)]
struct RequestSequence(u64);

impl RequestSequence {
    fn next(&mut self) -> u64 {
        let request_id = self.0;
        self.0 = self.0.wrapping_add(1);
        request_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThreadPageRequest {
    request_id: u64,
    account_id: MailAccountId,
    folder_id: FolderId,
    offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchRequest {
    request_id: u64,
    account_id: MailAccountId,
    query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountActivationRequest {
    request_id: u64,
    account_id: MailAccountId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxReloadRequest {
    request_id: u64,
    account_id: MailAccountId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageDetailRequest {
    request_id: u64,
    account_id: MailAccountId,
    conversation_id: ConversationId,
}

pub struct AccountActivationLoad {
    pub service: MailService,
    pub request_id: u64,
    pub account_id: MailAccountId,
    pub conversation_limit: usize,
}

pub struct ThreadPageLoad {
    pub service: MailService,
    pub request_id: u64,
    pub account_id: MailAccountId,
    pub folder_id: FolderId,
    pub offset: usize,
    pub limit: usize,
}

pub struct MailboxReloadLoad {
    pub service: MailService,
    pub request_id: u64,
    pub account_id: MailAccountId,
    pub selected_folder_id: Option<FolderId>,
    pub conversation_limit: usize,
}

pub struct SearchLoad {
    pub service: MailService,
    pub request_id: u64,
    pub account_id: MailAccountId,
    pub query: String,
}

pub struct MessageDetailLoad {
    pub service: MailService,
    pub request_id: u64,
    pub account_id: MailAccountId,
    pub conversation_id: ConversationId,
}

pub struct AccountRefreshRequest {
    pub service: MailService,
    pub account_id: MailAccountId,
}

pub struct MessageActionRequest {
    pub service: MailService,
    pub account_id: MailAccountId,
    pub conversation_id: ConversationId,
    pub action: MessageAction,
}

impl MailboxViewModel {
    pub fn loading_placeholder(backend: SharedMailBackend) -> Self {
        Self {
            service: MailService::new(backend),
            mailbox_mode: MailboxMode::Loading,
            accounts: Vec::new(),
            selected_account: 0,
            folders: Vec::new(),
            selected_folder: 0,
            threads: Vec::new(),
            has_more_threads: false,
            thread_page_offset: 0,
            pending_thread_page: None,
            pending_search: None,
            pending_account_activation: None,
            pending_mailbox_reload: None,
            pending_message_actions: std::collections::HashSet::new(),
            refresh_failures: std::collections::HashMap::new(),
            selected_thread: None,
            message_detail: None,
            message_detail_loading: false,
            pending_message_detail: None,
            request_ids: RequestSequence::default(),
            message_detail_error: None,
            search_query: String::new(),
            search_error: None,
            prefer_html_view: true,
        }
    }

    pub fn is_bootstrap_placeholder(&self) -> bool {
        self.is_loading() && self.accounts.is_empty()
    }

    pub fn is_loading(&self) -> bool {
        self.mailbox_mode == MailboxMode::Loading
    }

    pub fn from_snapshot(
        accounts: Vec<MailAccount>,
        settings: AppSettings,
        backend: SharedMailBackend,
        snapshot: AccountMailboxSnapshot,
    ) -> Self {
        let selected_account = settings
            .selected_account_id
            .as_deref()
            .and_then(|selected| accounts.iter().position(|account| account.id.0 == selected))
            .unwrap_or(0);
        let prefer_html_view = settings.prefer_html_view;
        let AccountMailboxSnapshot {
            mode,
            folders,
            conversations,
        } = snapshot;
        let thread_page_offset = conversations.len();
        let has_more_threads = thread_page_offset == CONVERSATION_PAGE_SIZE;
        Self {
            service: MailService::new(backend),
            mailbox_mode: mode,
            accounts,
            selected_account,
            folders,
            selected_folder: 0,
            threads: conversations,
            has_more_threads,
            thread_page_offset,
            pending_thread_page: None,
            pending_search: None,
            pending_account_activation: None,
            pending_mailbox_reload: None,
            pending_message_actions: std::collections::HashSet::new(),
            refresh_failures: std::collections::HashMap::new(),
            selected_thread: None,
            message_detail: None,
            message_detail_loading: false,
            pending_message_detail: None,
            request_ids: RequestSequence::default(),
            message_detail_error: None,
            search_query: String::new(),
            search_error: None,
            prefer_html_view,
        }
    }

    pub fn backend_summary(&self) -> String {
        if self.is_loading() {
            return "Loading mail…".into();
        }
        if let Some(failure) = self
            .current_account_id()
            .and_then(|account_id| self.refresh_failures.get(&account_id).copied())
        {
            return failure.status_message().into();
        }
        if self.current_eds_binding().is_some() {
            return "EDS/Camel".into();
        }
        match self.mailbox_mode {
            MailboxMode::Loading => "Loading…".into(),
            MailboxMode::StubNoAccount => "Stub/No account".into(),
            MailboxMode::StubUnavailable => "Stub/Unavailable".into(),
            MailboxMode::Live => "Stub/Unavailable".into(),
        }
    }

    pub fn current_eds_binding(&self) -> Option<crate::integration::backend::EdsAccountBinding> {
        let account = self.current_account()?;
        self.service.eds_binding(&account.id)
    }

    pub(crate) fn retained_backend(&self) -> Option<SharedMailBackend> {
        self.current_account_id()
            .filter(|account_id| !crate::integration::stub::is_stub_account_id(account_id))
            .map(|_| self.service.backend())
    }

    pub fn can_compose(&self) -> bool {
        let has_identity = self
            .current_account()
            .and_then(|account| account.default_identity())
            .is_some();
        has_identity && !matches!(self.mailbox_mode, MailboxMode::Loading)
    }

    pub fn current_account(&self) -> Option<&MailAccount> {
        self.accounts.get(self.selected_account)
    }

    pub fn current_account_id(&self) -> Option<MailAccountId> {
        self.current_account().map(|account| account.id.clone())
    }

    pub fn current_account_refresh_handle(&self) -> Option<AccountRefreshRequest> {
        if self.pending_account_activation.is_some() {
            return None;
        }
        let account_id = self.current_account_id()?;
        self.service.eds_binding(&account_id)?;
        Some(AccountRefreshRequest {
            service: self.service.clone(),
            account_id,
        })
    }

    pub fn account_display_name(&self, account_id: &MailAccountId) -> Option<&str> {
        self.account_index(account_id)
            .and_then(|index| self.accounts.get(index))
            .map(|account| account.display_name.as_str())
    }

    pub fn account_index(&self, account_id: &MailAccountId) -> Option<usize> {
        self.accounts
            .iter()
            .position(|account| account.id == *account_id)
    }

    pub fn set_prefer_html_view(&mut self, prefer_html_view: bool) {
        self.prefer_html_view = prefer_html_view;
    }

    pub fn prefer_html_view(&self) -> bool {
        self.prefer_html_view
    }

    pub fn current_folder(&self) -> Option<&MailFolder> {
        self.folders.get(self.selected_folder)
    }

    pub fn current_thread(&self) -> Option<&ConversationSummary> {
        let selected = self.selected_thread.as_ref()?;
        self.threads.iter().find(|thread| thread.id == *selected)
    }

    pub fn select_thread(&mut self, conversation_id: ConversationId) {
        if self.selected_thread.as_ref() == Some(&conversation_id) {
            return;
        }
        self.selected_thread = Some(conversation_id);
        self.message_detail = None;
        self.message_detail_loading = false;
        self.pending_message_detail = None;
        self.message_detail_error = None;
    }

    pub fn begin_account_activation(&mut self, index: usize) -> Option<AccountActivationLoad> {
        if index >= self.accounts.len() || index == self.selected_account {
            return None;
        }
        let account_id = self.accounts[index].id.clone();
        let request_id = self.request_ids.next();
        self.pending_account_activation = Some(AccountActivationRequest {
            request_id,
            account_id: account_id.clone(),
        });
        self.selected_account = index;
        self.selected_folder = 0;
        self.search_query.clear();
        self.search_error = None;
        self.pending_search = None;
        self.folders.clear();
        self.threads.clear();
        self.has_more_threads = false;
        self.thread_page_offset = 0;
        self.pending_thread_page = None;
        self.pending_mailbox_reload = None;
        self.clear_message_selection();
        self.mailbox_mode = MailboxMode::Loading;
        Some(AccountActivationLoad {
            service: self.service.clone(),
            request_id,
            account_id,
            conversation_limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub fn finish_account_activation(
        &mut self,
        request_id: u64,
        account_id: &MailAccountId,
        result: Result<AccountMailboxSnapshot, String>,
    ) -> bool {
        let expected = AccountActivationRequest {
            request_id,
            account_id: account_id.clone(),
        };
        if self.pending_account_activation.as_ref() != Some(&expected)
            || self.current_account_id().as_ref() != Some(account_id)
        {
            return false;
        }
        self.pending_account_activation = None;
        match result {
            Ok(snapshot) => {
                self.mailbox_mode = snapshot.mode;
                self.folders = snapshot.folders;
                self.threads = snapshot.conversations;
                self.has_more_threads = self.threads.len() == CONVERSATION_PAGE_SIZE;
                self.thread_page_offset = self.threads.len();
                self.search_error = None;
            }
            Err(error) => {
                self.mailbox_mode = MailboxMode::StubUnavailable;
                self.folders.clear();
                self.threads.clear();
                self.has_more_threads = false;
                self.thread_page_offset = 0;
                self.search_error = Some(error);
            }
        }
        true
    }

    pub fn update_identity(
        &mut self,
        account_index: usize,
        alias_index: usize,
        username: String,
        address: String,
        reply_to: Option<String>,
        signature_text: String,
    ) {
        let Some(account) = self.accounts.get_mut(account_index) else {
            return;
        };
        let Some(identity) = account.aliases.get_mut(alias_index) else {
            return;
        };

        identity.display_name = username.clone();
        if !identity.is_primary_address && !address.trim().is_empty() {
            identity.address = address.trim().to_string();
        }
        identity.reply_to = reply_to.clone().filter(|value| !value.is_empty());
        identity.signature_text = signature_text.clone();
        identity.signature_html = glib::markup_escape_text(&signature_text)
            .to_string()
            .replace('\n', "<br>");
    }

    pub fn update_account_name(&mut self, account_index: usize, account_name: String) {
        let Some(account) = self.accounts.get_mut(account_index) else {
            return;
        };
        if !account_name.trim().is_empty() {
            account.display_name = account_name.trim().to_string();
        }
    }

    pub fn set_default_identity(&mut self, account_index: usize, alias_index: usize) {
        let Some(account) = self.accounts.get_mut(account_index) else {
            return;
        };
        for (index, identity) in account.aliases.iter_mut().enumerate() {
            identity.is_default = index == alias_index;
        }
    }

    pub fn eds_binding(
        &self,
        account_id: &MailAccountId,
    ) -> Option<crate::integration::backend::EdsAccountBinding> {
        self.service.eds_binding(account_id)
    }

    pub fn account_identity(
        &self,
        account_index: usize,
        alias_index: usize,
    ) -> Option<SendingIdentity> {
        self.accounts
            .get(account_index)
            .and_then(|account| account.aliases.get(alias_index))
            .cloned()
    }

    pub fn add_identity(&mut self, account_index: usize) -> Option<usize> {
        let account = self.accounts.get_mut(account_index)?;
        let primary = account.primary_or_first_identity().cloned();
        let template = primary?;
        let address = template.address;
        let display_name = template.display_name;
        account.aliases.push(SendingIdentity::new(
            &account.id.0,
            address,
            display_name,
            None,
            String::new(),
            String::new(),
            false,
        ));
        Some(account.aliases.len() - 1)
    }
    pub fn remove_identity(&mut self, account_index: usize, alias_index: usize) -> bool {
        let Some(account) = self.accounts.get_mut(account_index) else {
            return false;
        };
        let Some(identity) = account.aliases.get(alias_index) else {
            return false;
        };
        if identity.is_primary_address {
            return false;
        }
        let removed_default = identity.is_default;
        account.aliases.remove(alias_index);
        if removed_default {
            if let Some(primary) = account.primary_identity_mut() {
                primary.is_default = true;
            } else if let Some(first) = account.aliases.first_mut() {
                first.is_default = true;
            }
        }
        true
    }

    pub fn select_folder(&mut self, index: usize) -> Option<ThreadPageLoad> {
        if index >= self.folders.len() || index == self.selected_folder {
            return None;
        }
        self.pending_mailbox_reload = None;
        self.clear_message_selection();
        self.selected_folder = index;
        self.threads.clear();
        self.has_more_threads = true;
        self.thread_page_offset = 0;
        self.pending_thread_page = None;
        self.begin_thread_page_load_at(0)
    }

    pub fn search(&mut self, query: &str) -> Option<SearchLoad> {
        let query = query.trim().to_string();
        if self.search_query == query {
            return None;
        }
        self.search_query = query.clone();
        self.clear_message_selection();
        self.pending_thread_page = None;
        self.pending_mailbox_reload = None;
        self.has_more_threads = false;
        self.search_error = None;

        if query.is_empty() {
            self.pending_search = None;
            self.threads.clear();
            self.thread_page_offset = 0;
            self.has_more_threads = true;
            return None;
        }

        self.threads.clear();
        self.thread_page_offset = 0;
        let account_id = self.current_account()?.id.clone();
        let request_id = self.request_ids.next();
        self.pending_search = Some(SearchRequest {
            request_id,
            account_id: account_id.clone(),
            query: query.clone(),
        });
        Some(SearchLoad {
            service: self.service.clone(),
            request_id,
            account_id,
            query,
        })
    }

    pub fn finish_search(
        &mut self,
        request_id: u64,
        account_id: &MailAccountId,
        query: &str,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> bool {
        let request = SearchRequest {
            request_id,
            account_id: account_id.clone(),
            query: query.to_string(),
        };
        if self.pending_search.as_ref() != Some(&request)
            || self.current_account_id().as_ref() != Some(account_id)
            || self.search_query != query
        {
            return false;
        }
        self.pending_search = None;
        match result {
            Ok(mut results) => {
                results.sort_by(|left, right| {
                    right
                        .last_updated_unix_ms
                        .cmp(&left.last_updated_unix_ms)
                        .then_with(|| left.subject.cmp(&right.subject))
                });
                let mut seen = std::collections::HashSet::new();
                results.retain(|summary| seen.insert(summary.id.clone()));
                self.threads = results;
                self.search_error = None;
            }
            Err(error) => {
                self.threads.clear();
                self.search_error = Some(error);
            }
        }
        self.thread_page_offset = self.threads.len();
        true
    }

    pub fn finish_account_refresh(
        &mut self,
        refreshed_account_id: &MailAccountId,
        failure: Option<RefreshFailureKind>,
    ) -> Option<MailboxReloadLoad> {
        if let Some(failure) = failure {
            self.set_refresh_failure(refreshed_account_id.clone(), failure);
        } else {
            self.refresh_failures.remove(refreshed_account_id);
        }
        if self.current_account_id().as_ref() != Some(refreshed_account_id) {
            return None;
        }
        self.begin_mailbox_reload()
    }

    pub(crate) fn set_refresh_failure(
        &mut self,
        account_id: MailAccountId,
        failure: RefreshFailureKind,
    ) {
        self.refresh_failures.insert(account_id, failure);
    }

    pub fn begin_cache_change_reload(
        &mut self,
        changed_account_id: &MailAccountId,
    ) -> Option<MailboxReloadLoad> {
        if self.current_account_id().as_ref() != Some(changed_account_id) {
            return None;
        }
        self.begin_mailbox_reload()
    }

    fn begin_mailbox_reload(&mut self) -> Option<MailboxReloadLoad> {
        if self.is_loading() || !self.search_query.is_empty() {
            return None;
        }
        let account_id = self.current_account_id()?;
        let request_id = self.request_ids.next();
        self.pending_mailbox_reload = Some(MailboxReloadRequest {
            request_id,
            account_id: account_id.clone(),
        });
        Some(MailboxReloadLoad {
            service: self.service.clone(),
            request_id,
            account_id,
            selected_folder_id: self.current_folder().map(|folder| folder.id.clone()),
            conversation_limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub fn finish_mailbox_reload(
        &mut self,
        request_id: u64,
        account_id: &MailAccountId,
        result: Result<MailboxContentSnapshot, String>,
    ) -> bool {
        let request = MailboxReloadRequest {
            request_id,
            account_id: account_id.clone(),
        };
        if self.pending_mailbox_reload.as_ref() != Some(&request)
            || self.current_account_id().as_ref() != Some(account_id)
        {
            return false;
        }
        self.pending_mailbox_reload = None;
        let Ok(snapshot) = result else {
            return false;
        };

        let previous_selection = self.selected_thread.clone();
        let previous_detail = self.message_detail.clone();
        self.folders = snapshot.folders;
        self.selected_folder = snapshot
            .selected_folder_id
            .as_ref()
            .and_then(|folder_id| {
                self.folders
                    .iter()
                    .position(|folder| folder.id == *folder_id)
            })
            .unwrap_or(0);
        self.threads = snapshot.conversations;
        self.thread_page_offset = self.threads.len();
        self.has_more_threads = self.threads.len() == CONVERSATION_PAGE_SIZE;
        self.pending_thread_page = None;
        self.selected_thread = previous_selection
            .filter(|selected| self.threads.iter().any(|thread| thread.id == *selected));
        self.message_detail = self.selected_thread.as_ref().and_then(|selected| {
            previous_detail.filter(|detail| detail.conversation_id == *selected)
        });
        if self.selected_thread.is_none() {
            self.clear_message_selection();
        }
        true
    }

    pub fn can_load_more_threads(&self) -> bool {
        self.has_more_threads && self.pending_thread_page.is_none()
    }

    pub fn search_loading(&self) -> bool {
        self.pending_search.is_some()
    }

    pub fn initial_threads_loading(&self) -> bool {
        self.pending_thread_page
            .as_ref()
            .is_some_and(|request| request.offset == 0 && self.threads.is_empty())
    }

    pub fn begin_thread_page_load(&mut self) -> Option<ThreadPageLoad> {
        if !self.can_load_more_threads() {
            return None;
        }

        self.begin_thread_page_load_at(self.thread_page_offset)
    }

    fn begin_thread_page_load_at(&mut self, offset: usize) -> Option<ThreadPageLoad> {
        let account_id = self.current_account()?.id.clone();
        let folder_id = self.current_folder()?.id.clone();
        let request_id = self.request_ids.next();
        self.pending_thread_page = Some(ThreadPageRequest {
            request_id,
            account_id: account_id.clone(),
            folder_id: folder_id.clone(),
            offset,
        });
        Some(ThreadPageLoad {
            service: self.service.clone(),
            request_id,
            account_id,
            folder_id,
            offset,
            limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub fn finish_thread_page_load(
        &mut self,
        request_id: u64,
        account_id: &MailAccountId,
        folder_id: &FolderId,
        offset: usize,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> Option<Vec<ConversationSummary>> {
        let request = ThreadPageRequest {
            request_id,
            account_id: account_id.clone(),
            folder_id: folder_id.clone(),
            offset,
        };
        if self.pending_thread_page.as_ref() != Some(&request) {
            return None;
        }
        self.pending_thread_page = None;

        let next_batch = match result {
            Ok(batch) => batch,
            Err(_) => {
                if offset == 0 {
                    self.has_more_threads = false;
                }
                return Some(Vec::new());
            }
        };
        self.thread_page_offset = offset.saturating_add(next_batch.len());
        self.has_more_threads = next_batch.len() == CONVERSATION_PAGE_SIZE;

        let mut existing_ids = self
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let added = next_batch
            .into_iter()
            .filter(|thread| existing_ids.insert(thread.id.clone()))
            .collect::<Vec<_>>();
        self.threads.extend(added.iter().cloned());
        Some(added)
    }

    pub fn begin_toggle_star(&mut self) -> Option<MessageActionRequest> {
        let starred = self
            .message_detail
            .as_ref()
            .map(|detail| detail.starred)
            .or_else(|| self.current_thread().map(|thread| thread.starred))?;
        self.begin_message_action(MessageAction::SetStarred(!starred))
    }

    pub fn begin_toggle_read(&mut self) -> Option<MessageActionRequest> {
        let unread = self
            .message_detail
            .as_ref()
            .map(|detail| detail.unread)
            .or_else(|| self.current_thread().map(|thread| thread.unread_count > 0))?;
        self.begin_message_action(MessageAction::SetRead(unread))
    }

    pub fn begin_archive_selected(&mut self) -> Option<MessageActionRequest> {
        self.begin_message_action(MessageAction::MoveTo(FolderId("archive".into())))
    }

    pub fn begin_trash_selected(&mut self) -> Option<MessageActionRequest> {
        self.begin_message_action(MessageAction::MoveTo(FolderId("trash".into())))
    }

    pub(crate) fn mail_service(&self) -> MailService {
        self.service.clone()
    }

    pub fn selected_message_action_pending(&self) -> bool {
        let Some(account_id) = self.current_account_id() else {
            return false;
        };
        let Some(conversation_id) = self.selected_thread.as_ref() else {
            return false;
        };
        self.pending_message_actions
            .contains(&(account_id, conversation_id.clone()))
    }

    fn begin_message_action(&mut self, action: MessageAction) -> Option<MessageActionRequest> {
        let account_id = self.current_account_id()?;
        let conversation_id = self.selected_thread.clone()?;
        if !self
            .pending_message_actions
            .insert((account_id.clone(), conversation_id.clone()))
        {
            return None;
        }
        Some(MessageActionRequest {
            service: self.service.clone(),
            account_id,
            conversation_id,
            action,
        })
    }

    pub fn finish_message_action(
        &mut self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        action: &MessageAction,
        result: &Result<(), String>,
    ) -> bool {
        self.pending_message_actions
            .remove(&(account_id.clone(), conversation_id.clone()));
        if result.is_err() || self.current_account_id().as_ref() != Some(account_id) {
            return false;
        }
        if matches!(
            self.mailbox_mode,
            MailboxMode::StubNoAccount | MailboxMode::StubUnavailable
        ) {
            return true;
        }

        match action {
            MessageAction::SetStarred(starred) => {
                if let Some(thread) = self
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == *conversation_id)
                {
                    thread.starred = *starred;
                }
                if let Some(detail) = self
                    .message_detail
                    .as_mut()
                    .filter(|detail| detail.conversation_id == *conversation_id)
                {
                    detail.starred = *starred;
                }
            }
            MessageAction::SetRead(read) => {
                let was_unread = self
                    .threads
                    .iter()
                    .find(|thread| thread.id == *conversation_id)
                    .map(|thread| thread.unread_count > 0);
                if let Some(thread) = self
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == *conversation_id)
                {
                    thread.unread_count = u32::from(!read);
                }
                if let Some(detail) = self
                    .message_detail
                    .as_mut()
                    .filter(|detail| detail.conversation_id == *conversation_id)
                {
                    detail.unread = !read;
                }
                if let (Some(was_unread), Some(folder)) =
                    (was_unread, self.folders.get_mut(self.selected_folder))
                {
                    match (was_unread, !read) {
                        (false, true) => {
                            folder.unread_count = folder.unread_count.saturating_add(1)
                        }
                        (true, false) => {
                            folder.unread_count = folder.unread_count.saturating_sub(1)
                        }
                        _ => {}
                    }
                }
            }
            MessageAction::MoveTo(_) => {
                let moved_unread = self
                    .threads
                    .iter()
                    .find(|thread| thread.id == *conversation_id)
                    .map(|thread| thread.unread_count)
                    .unwrap_or(0);
                let previous_len = self.threads.len();
                self.threads.retain(|thread| thread.id != *conversation_id);
                let removed = self.threads.len() != previous_len;
                if let Some(folder) = self.folders.get_mut(self.selected_folder) {
                    folder.unread_count = folder.unread_count.saturating_sub(moved_unread);
                }
                if self.selected_thread.as_ref() == Some(conversation_id) {
                    self.selected_thread = self.threads.first().map(|thread| thread.id.clone());
                    self.message_detail = None;
                    self.message_detail_loading = false;
                    self.pending_message_detail = None;
                    self.message_detail_error = None;
                }
                if removed {
                    self.thread_page_offset = self.thread_page_offset.saturating_sub(1);
                }
            }
        }
        true
    }

    fn clear_message_selection(&mut self) {
        self.selected_thread = None;
        self.message_detail = None;
        self.message_detail_loading = false;
        self.pending_message_detail = None;
        self.message_detail_error = None;
    }

    pub fn begin_message_detail_load(&mut self) -> Option<MessageDetailLoad> {
        let account_id = self.current_account()?.id.clone();
        let conversation_id = self.selected_thread.clone()?;
        if self
            .message_detail
            .as_ref()
            .map(|detail| detail.conversation_id == conversation_id)
            .unwrap_or(false)
        {
            self.message_detail_loading = false;
            self.pending_message_detail = None;
            self.message_detail_error = None;
            return None;
        }
        if self.pending_message_detail.is_some() {
            return None;
        }

        let request_id = self.request_ids.next();
        self.pending_message_detail = Some(MessageDetailRequest {
            request_id,
            account_id: account_id.clone(),
            conversation_id: conversation_id.clone(),
        });
        self.message_detail = None;
        self.message_detail_loading = true;
        self.message_detail_error = None;
        Some(MessageDetailLoad {
            service: self.service.clone(),
            request_id,
            account_id,
            conversation_id,
        })
    }

    pub fn finish_message_detail_load(
        &mut self,
        request_id: u64,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        result: Result<Option<MessageDetail>, String>,
    ) -> bool {
        let request = MessageDetailRequest {
            request_id,
            account_id: account_id.clone(),
            conversation_id: conversation_id.clone(),
        };
        if self.pending_message_detail.as_ref() != Some(&request)
            || self.current_account_id().as_ref() != Some(account_id)
            || self.selected_thread.as_ref() != Some(conversation_id)
        {
            return false;
        }

        self.pending_message_detail = None;
        self.message_detail_loading = false;
        match result {
            Ok(Some(detail)) if detail.conversation_id == *conversation_id => {
                self.message_detail = Some(detail);
                self.message_detail_error = None;
            }
            Ok(Some(_)) => {
                self.message_detail = None;
                self.message_detail_error = Some("Message unavailable.".into());
            }
            Ok(None) => {
                self.message_detail = None;
                self.message_detail_error = Some("Message unavailable.".into());
            }
            Err(error) => {
                self.message_detail = None;
                self.message_detail_error = Some(error);
            }
        }
        true
    }

    pub fn current_attachment_request(
        &self,
    ) -> Option<(MailService, MailAccountId, ConversationId)> {
        let account = self.current_account()?;
        let conversation_id = self.selected_thread.as_ref()?;
        Some((
            self.service.clone(),
            account.id.clone(),
            conversation_id.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::account::{AliasId, SendingIdentity};
    use crate::model::mail::{FolderKind, MessageId};

    fn account(id: &str) -> MailAccount {
        MailAccount {
            id: MailAccountId(id.into()),
            display_name: id.into(),
            aliases: vec![SendingIdentity::with_id(
                AliasId(format!("{id}:primary")),
                format!("{id}@example.com"),
                id.into(),
                None,
                String::new(),
                String::new(),
                true,
                true,
            )],
        }
    }

    fn loaded_mailbox(
        mut accounts: Vec<MailAccount>,
        settings: AppSettings,
        backend: SharedMailBackend,
        mailbox_mode: MailboxMode,
    ) -> MailboxViewModel {
        if accounts.is_empty() {
            accounts.push(crate::integration::stub::stub_account());
        }
        let selected_account = settings
            .selected_account_id
            .as_deref()
            .and_then(|selected| accounts.iter().find(|account| account.id.0 == selected))
            .unwrap_or(&accounts[0]);
        let mail_service = MailService::new(backend.clone());
        let folders =
            futures::executor::block_on(mail_service.list_folders(&selected_account.id)).unwrap();
        let conversations = folders
            .first()
            .map(|folder| {
                futures::executor::block_on(mail_service.list_conversations(
                    &selected_account.id,
                    &folder.id,
                    0,
                    CONVERSATION_PAGE_SIZE,
                ))
                .unwrap()
            })
            .unwrap_or_default();
        MailboxViewModel::from_snapshot(
            accounts,
            settings,
            backend,
            AccountMailboxSnapshot {
                mode: mailbox_mode,
                folders,
                conversations,
            },
        )
    }

    fn mailbox_for_detail_tests() -> MailboxViewModel {
        let mut mailbox =
            MailboxViewModel::loading_placeholder(crate::integration::backend::stub_backend());
        mailbox.accounts = vec![account("account-1"), account("account-2")];
        mailbox.selected_account = 0;
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox
            .begin_message_detail_load()
            .expect("detail fixture should start one request");
        mailbox
    }

    fn detail(conversation_id: &str) -> MessageDetail {
        MessageDetail {
            message_id: MessageId("message-1".into()),
            conversation_id: ConversationId(conversation_id.into()),
            subject: "Subject".into(),
            from: "sender@example.com".into(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: None,
            date_label: String::new(),
            starred: false,
            unread: false,
            attachments: Vec::new(),
            body: crate::model::mail::MessageBody::Empty,
        }
    }

    fn summary(index: usize) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(format!("conversation-{index}")),
            subject: format!("Subject {index}"),
            participants: vec!["sender@example.com".into()],
            message_count: 1,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: index as i64,
            preview: String::new(),
        }
    }

    fn mailbox_for_pagination_tests() -> MailboxViewModel {
        let mut mailbox =
            MailboxViewModel::loading_placeholder(crate::integration::backend::stub_backend());
        mailbox.accounts = vec![account("account-1")];
        mailbox.folders = vec![MailFolder {
            id: FolderId("inbox".into()),
            name: "Inbox".into(),
            unread_count: 0,
            kind: FolderKind::Inbox,
        }];
        mailbox.threads = (0..CONVERSATION_PAGE_SIZE).map(summary).collect();
        mailbox.has_more_threads = true;
        mailbox.thread_page_offset = CONVERSATION_PAGE_SIZE;
        mailbox.mailbox_mode = MailboxMode::Live;
        mailbox
    }

    fn mailbox_for_action_tests() -> MailboxViewModel {
        let mut mailbox = mailbox_for_pagination_tests();
        mailbox.selected_thread = Some(ConversationId("conversation-0".into()));
        mailbox.message_detail = Some(detail("conversation-0"));
        mailbox
    }

    #[test]
    fn no_eligible_eds_account_opens_a_small_stub_mailbox() {
        let mailbox = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            crate::integration::backend::stub_backend(),
            MailboxMode::StubNoAccount,
        );

        assert_eq!(mailbox.accounts.len(), 1);
        assert_eq!(mailbox.accounts[0].display_name, "Pigeon Mail Stub");
        assert_eq!(mailbox.threads.len(), 2);
        assert!(
            mailbox
                .threads
                .iter()
                .all(|thread| thread.subject.contains("Pigeon")
                    || thread.subject.contains("mailbox"))
        );
        assert_eq!(mailbox.backend_summary(), "Stub/No account");
        assert!(mailbox.can_compose());
        assert!(mailbox.current_account_refresh_handle().is_none());
        assert!(mailbox.retained_backend().is_none());
    }

    #[test]
    fn real_unavailable_accounts_retain_their_backend() {
        let backend = crate::integration::backend::lazy_mail_backend();
        let mailbox = MailboxViewModel::from_snapshot(
            vec![account("account-1")],
            AppSettings::default(),
            backend.clone(),
            AccountMailboxSnapshot {
                mode: MailboxMode::StubUnavailable,
                folders: Vec::new(),
                conversations: Vec::new(),
            },
        );

        let retained = mailbox
            .retained_backend()
            .expect("a real account keeps its lazy backend while unavailable");
        assert!(std::sync::Arc::ptr_eq(&retained, &backend));
    }

    #[test]
    fn snapshot_construction_is_backend_free_and_derives_pagination_from_the_page() {
        for conversation_count in [0, CONVERSATION_PAGE_SIZE - 1, CONVERSATION_PAGE_SIZE] {
            let folders = vec![MailFolder {
                id: FolderId("cached-inbox".into()),
                name: "Cached Inbox".into(),
                unread_count: 2,
                kind: FolderKind::Inbox,
            }];
            let conversations = (0..conversation_count).map(summary).collect::<Vec<_>>();
            let mailbox = MailboxViewModel::from_snapshot(
                vec![account("account-1"), account("account-2")],
                AppSettings {
                    selected_account_id: Some("account-2".into()),
                    ..AppSettings::default()
                },
                crate::integration::backend::lazy_mail_backend(),
                AccountMailboxSnapshot {
                    mode: MailboxMode::Live,
                    folders: folders.clone(),
                    conversations: conversations.clone(),
                },
            );

            assert_eq!(mailbox.current_account_id().unwrap().0, "account-2");
            assert_eq!(mailbox.folders.len(), 1);
            assert_eq!(mailbox.folders[0].id.0, "cached-inbox");
            assert_eq!(mailbox.threads.len(), conversation_count);
            assert_eq!(
                mailbox
                    .threads
                    .iter()
                    .map(|thread| thread.id.clone())
                    .collect::<Vec<_>>(),
                conversations
                    .iter()
                    .map(|thread| thread.id.clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(mailbox.thread_page_offset, conversation_count);
            assert_eq!(
                mailbox.can_load_more_threads(),
                conversation_count == CONVERSATION_PAGE_SIZE
            );
        }
    }

    #[test]
    fn html_view_preference_loads_and_can_change_in_both_directions() {
        for prefer_html_view in [false, true] {
            let settings = AppSettings {
                prefer_html_view,
                ..AppSettings::default()
            };
            let mut mailbox = loaded_mailbox(
                Vec::new(),
                settings,
                crate::integration::backend::stub_backend(),
                MailboxMode::StubNoAccount,
            );

            assert_eq!(mailbox.prefer_html_view, prefer_html_view);

            mailbox.set_prefer_html_view(!prefer_html_view);
            assert_eq!(mailbox.prefer_html_view, !prefer_html_view);
            mailbox.set_prefer_html_view(prefer_html_view);
            assert_eq!(mailbox.prefer_html_view, prefer_html_view);
        }
    }

    #[test]
    fn stub_summary_is_user_facing_and_hides_backend_topology() {
        let mut mailbox =
            MailboxViewModel::loading_placeholder(crate::integration::backend::stub_backend());
        mailbox.accounts = vec![account("account-1")];
        mailbox.mailbox_mode = MailboxMode::StubUnavailable;

        let summary = mailbox.backend_summary();
        assert_eq!(summary, "Stub/Unavailable");
        assert!(!summary.contains("uid"));
        assert!(!summary.contains("auth"));
        assert!(mailbox.can_compose());
    }

    #[test]
    fn account_activation_failure_reports_an_unavailable_stub() {
        let mut mailbox =
            MailboxViewModel::loading_placeholder(crate::integration::backend::stub_backend());
        mailbox.accounts = vec![account("account-1")];
        mailbox.mailbox_mode = MailboxMode::StubUnavailable;

        assert_eq!(mailbox.backend_summary(), "Stub/Unavailable");
        assert!(mailbox.can_compose());
    }

    #[test]
    fn unavailable_stub_keeps_no_op_writes_for_ui_exploration() {
        let mailbox = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            crate::integration::backend::stub_backend(),
            MailboxMode::StubUnavailable,
        );
        let account_id = mailbox.current_account_id().unwrap();

        assert!(mailbox.can_compose());
        assert_eq!(account_id.0, "local-stub");
    }

    #[test]
    fn only_the_empty_loading_model_is_a_bootstrap_placeholder() {
        let backend = crate::integration::backend::stub_backend();
        let placeholder = MailboxViewModel::loading_placeholder(backend.clone());
        assert!(placeholder.is_bootstrap_placeholder());

        let no_account = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            backend.clone(),
            MailboxMode::StubNoAccount,
        );
        assert!(!no_account.is_bootstrap_placeholder());

        let unavailable = loaded_mailbox(
            vec![account("account-1")],
            AppSettings::default(),
            backend.clone(),
            MailboxMode::StubUnavailable,
        );
        assert!(!unavailable.is_loading());
        assert_eq!(unavailable.backend_summary(), "Stub/Unavailable");
        assert!(!unavailable.is_bootstrap_placeholder());

        let mut activating = MailboxViewModel::loading_placeholder(backend);
        activating.accounts = vec![account("account-1")];
        assert!(activating.is_loading());
        assert!(!activating.is_bootstrap_placeholder());
    }

    #[test]
    fn account_activation_is_scoped_and_installs_the_loaded_snapshot() {
        let mut mailbox = mailbox_for_detail_tests();

        assert!(mailbox.begin_account_activation(0).is_none());
        assert!(mailbox.begin_account_activation(usize::MAX).is_none());
        let request = mailbox.begin_account_activation(1).unwrap();
        assert_eq!(request.account_id.0, "account-2");
        assert!(mailbox.is_loading());
        assert!(mailbox.folders.is_empty());
        assert!(mailbox.threads.is_empty());
        assert!(mailbox.selected_thread.is_none());

        assert!(!mailbox.finish_account_activation(
            request.request_id,
            &MailAccountId("account-1".into()),
            Ok(AccountMailboxSnapshot {
                mode: MailboxMode::Live,
                folders: Vec::new(),
                conversations: Vec::new(),
            }),
        ));

        let folders = vec![MailFolder {
            id: FolderId("inbox".into()),
            name: "Inbox".into(),
            unread_count: 1,
            kind: FolderKind::Inbox,
        }];
        assert!(mailbox.finish_account_activation(
            request.request_id,
            &request.account_id,
            Ok(AccountMailboxSnapshot {
                mode: MailboxMode::Live,
                folders: folders.clone(),
                conversations: vec![summary(42)],
            }),
        ));
        assert!(!mailbox.is_loading());
        assert_eq!(mailbox.folders.len(), 1);
        assert_eq!(mailbox.folders[0].id.0, "inbox");
        assert_eq!(mailbox.folders[0].unread_count, 1);
        assert_eq!(mailbox.threads[0].id.0, "conversation-42");
        assert_eq!(mailbox.thread_page_offset, 1);
    }

    #[test]
    fn newer_account_activation_supersedes_an_older_completion() {
        let mut mailbox = mailbox_for_detail_tests();
        let second_account = mailbox.begin_account_activation(1).unwrap();
        let first_account = mailbox.begin_account_activation(0).unwrap();

        assert!(!mailbox.finish_account_activation(
            second_account.request_id,
            &second_account.account_id,
            Ok(AccountMailboxSnapshot {
                mode: MailboxMode::Live,
                folders: Vec::new(),
                conversations: vec![summary(20)],
            }),
        ));
        assert!(mailbox.finish_account_activation(
            first_account.request_id,
            &first_account.account_id,
            Ok(AccountMailboxSnapshot {
                mode: MailboxMode::StubUnavailable,
                folders: Vec::new(),
                conversations: vec![summary(10)],
            }),
        ));
        assert_eq!(mailbox.current_account_id().unwrap().0, "account-1");
        assert_eq!(mailbox.threads[0].id.0, "conversation-10");
        assert_eq!(mailbox.mailbox_mode, MailboxMode::StubUnavailable);
    }

    #[test]
    fn failed_account_activation_ends_loading_without_exposing_backend_detail() {
        let mut mailbox = mailbox_for_detail_tests();
        let request = mailbox.begin_account_activation(1).unwrap();

        assert!(mailbox.finish_account_activation(
            request.request_id,
            &request.account_id,
            Err("Account unavailable.".into()),
        ));
        assert!(!mailbox.is_loading());
        assert_eq!(mailbox.mailbox_mode, MailboxMode::StubUnavailable);
        assert_eq!(
            mailbox.search_error.as_deref(),
            Some("Account unavailable.")
        );
        assert!(mailbox.folders.is_empty());
        assert!(mailbox.threads.is_empty());
    }

    #[test]
    fn changing_folder_clears_message_selection_and_detail_state() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        mailbox.message_detail_loading = true;
        mailbox.message_detail_error = Some("old detail failure".into());

        let request = mailbox
            .select_folder(1)
            .expect("a different folder should start a scoped load");

        assert_eq!(mailbox.selected_folder, 1);
        assert_eq!(request.folder_id.0, "archive");
        assert_eq!(request.offset, 0);
        assert!(mailbox.initial_threads_loading());
        assert!(mailbox.threads.is_empty());
        assert!(mailbox.selected_thread.is_none());
        assert!(mailbox.message_detail.is_none());
        assert!(!mailbox.message_detail_loading);
        assert!(mailbox.message_detail_error.is_none());

        let added = mailbox
            .finish_thread_page_load(
                request.request_id,
                &request.account_id,
                &request.folder_id,
                request.offset,
                Ok(vec![summary(7)]),
            )
            .expect("the current folder load should be accepted");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].id.0, "conversation-7");
        assert!(!mailbox.initial_threads_loading());
        assert_eq!(mailbox.threads[0].id.0, "conversation-7");
    }

    #[test]
    fn selecting_the_current_folder_does_not_discard_the_current_message() {
        let mut mailbox = mailbox_for_action_tests();

        assert!(mailbox.select_folder(0).is_none());

        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-0")
        );
        assert!(mailbox.message_detail.is_some());
    }

    #[test]
    fn rapid_folder_changes_reject_stale_results_and_stop_failed_initial_loading() {
        let mut mailbox = mailbox_for_action_tests();
        for (id, kind) in [
            ("archive", FolderKind::Archive),
            ("trash", FolderKind::Trash),
        ] {
            mailbox.folders.push(MailFolder {
                id: FolderId(id.into()),
                name: id.into(),
                unread_count: 0,
                kind,
            });
        }

        let archive = mailbox.select_folder(1).unwrap();
        let trash = mailbox.select_folder(2).unwrap();
        assert!(
            mailbox
                .finish_thread_page_load(
                    archive.request_id,
                    &archive.account_id,
                    &archive.folder_id,
                    archive.offset,
                    Ok(vec![summary(1)]),
                )
                .is_none()
        );
        assert_eq!(mailbox.selected_folder, 2);
        assert!(mailbox.threads.is_empty());

        assert!(
            mailbox
                .finish_thread_page_load(
                    trash.request_id,
                    &trash.account_id,
                    &trash.folder_id,
                    trash.offset,
                    Err("Messages unavailable.".into()),
                )
                .is_some()
        );
        assert!(!mailbox.initial_threads_loading());
        assert!(mailbox.begin_thread_page_load().is_none());
    }

    #[test]
    fn search_starts_an_async_request_and_clears_stale_rows() {
        let mut mailbox = mailbox_for_pagination_tests();

        let request = mailbox.search("  needle  ").unwrap();

        assert_eq!(request.request_id, 0);
        assert_eq!(request.account_id.0, "account-1");
        assert_eq!(request.query, "needle");
        assert_eq!(mailbox.search_query, "needle");
        assert!(mailbox.threads.is_empty());
        assert!(!mailbox.has_more_threads);
    }

    #[test]
    fn stale_search_completion_cannot_replace_a_newer_query() {
        let mut mailbox = mailbox_for_pagination_tests();
        let first = mailbox.search("first").unwrap();
        let second = mailbox.search("second").unwrap();

        assert!(!mailbox.finish_search(
            first.request_id,
            &first.account_id,
            &first.query,
            Ok(vec![summary(90)]),
        ));
        assert!(mailbox.threads.is_empty());
        assert!(mailbox.finish_search(
            second.request_id,
            &second.account_id,
            &second.query,
            Ok(vec![summary(91)]),
        ));
        assert_eq!(mailbox.threads[0].id.0, "conversation-91");
    }

    #[test]
    fn failed_search_is_scoped_and_exposes_a_safe_error() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.search("needle").unwrap();

        assert!(mailbox.finish_search(
            request.request_id,
            &request.account_id,
            &request.query,
            Err("offline".into()),
        ));
        assert_eq!(mailbox.search_query, "needle");
        assert_eq!(mailbox.search_error.as_deref(), Some("offline"));
        assert!(mailbox.threads.is_empty());
    }

    #[test]
    fn clearing_search_invalidates_pending_completion() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.search("needle").unwrap();

        assert!(mailbox.search("").is_none());
        assert!(mailbox.search_query.is_empty());
        assert!(mailbox.threads.is_empty());
        assert!(
            mailbox
                .begin_cache_change_reload(&request.account_id)
                .is_some()
        );
        assert!(!mailbox.finish_search(
            request.request_id,
            &request.account_id,
            &request.query,
            Ok(vec![summary(99)]),
        ));
        assert!(
            !mailbox
                .threads
                .iter()
                .any(|thread| thread.id.0 == "conversation-99")
        );
    }

    #[test]
    fn message_actions_are_single_flight_per_message() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox
            .begin_toggle_star()
            .expect("first action should start");

        assert_eq!(request.action, MessageAction::SetStarred(true));
        assert!(mailbox.selected_message_action_pending());
        assert!(mailbox.begin_toggle_read().is_none());

        assert!(mailbox.finish_message_action(
            &request.account_id,
            &request.conversation_id,
            &request.action,
            &Ok(()),
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert!(mailbox.current_thread().unwrap().starred);
        assert!(mailbox.message_detail.as_ref().unwrap().starred);
    }

    #[test]
    fn successful_stub_actions_release_the_request_without_mutating_the_placeholder() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.mailbox_mode = MailboxMode::StubUnavailable;
        let original_thread_count = mailbox.threads.len();
        let original_starred = mailbox.current_thread().unwrap().starred;
        let original_selection = mailbox.selected_thread.clone();

        let star = mailbox.begin_toggle_star().unwrap();
        assert!(mailbox.finish_message_action(
            &star.account_id,
            &star.conversation_id,
            &star.action,
            &Ok(()),
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert_eq!(mailbox.current_thread().unwrap().starred, original_starred);

        let move_request = mailbox.begin_trash_selected().unwrap();
        assert!(mailbox.finish_message_action(
            &move_request.account_id,
            &move_request.conversation_id,
            &move_request.action,
            &Ok(()),
        ));
        assert_eq!(mailbox.threads.len(), original_thread_count);
        assert_eq!(mailbox.selected_thread, original_selection);
    }

    #[test]
    fn failed_message_action_releases_pending_state_without_mutation() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox.begin_toggle_read().unwrap();

        assert!(!mailbox.finish_message_action(
            &request.account_id,
            &request.conversation_id,
            &request.action,
            &Err("offline".into()),
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert!(!mailbox.message_detail.as_ref().unwrap().unread);
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 0);
        assert!(mailbox.begin_toggle_read().is_some());
    }

    #[test]
    fn read_completion_keeps_message_thread_and_folder_counts_consistent() {
        let mut mailbox = mailbox_for_action_tests();
        let mark_unread = mailbox.begin_toggle_read().unwrap();

        assert!(mailbox.finish_message_action(
            &mark_unread.account_id,
            &mark_unread.conversation_id,
            &mark_unread.action,
            &Ok(()),
        ));
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 1);
        assert_eq!(mailbox.current_folder().unwrap().unread_count, 1);
        assert!(mailbox.message_detail.as_ref().unwrap().unread);

        let mark_read = mailbox.begin_toggle_read().unwrap();
        assert!(mailbox.finish_message_action(
            &mark_read.account_id,
            &mark_read.conversation_id,
            &mark_read.action,
            &Ok(()),
        ));
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 0);
        assert_eq!(mailbox.current_folder().unwrap().unread_count, 0);
        assert!(!mailbox.message_detail.as_ref().unwrap().unread);
    }

    #[test]
    fn late_action_updates_its_row_without_overwriting_newer_detail() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox.begin_toggle_star().unwrap();
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox.message_detail = Some(detail("conversation-1"));

        assert!(mailbox.finish_message_action(
            &request.account_id,
            &request.conversation_id,
            &request.action,
            &Ok(()),
        ));
        assert!(
            mailbox
                .threads
                .iter()
                .find(|thread| thread.id == request.conversation_id)
                .unwrap()
                .starred
        );
        assert_eq!(
            mailbox.message_detail.as_ref().unwrap().conversation_id.0,
            "conversation-1"
        );
        assert!(!mailbox.message_detail.as_ref().unwrap().starred);
    }

    #[test]
    fn late_move_does_not_replace_a_newer_selection() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox.begin_archive_selected().unwrap();
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox.message_detail = Some(detail("conversation-1"));

        assert!(mailbox.finish_message_action(
            &request.account_id,
            &request.conversation_id,
            &request.action,
            &Ok(()),
        ));
        assert!(
            !mailbox
                .threads
                .iter()
                .any(|thread| thread.id == request.conversation_id)
        );
        assert_eq!(mailbox.thread_page_offset, CONVERSATION_PAGE_SIZE - 1);
        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-1")
        );
        assert_eq!(
            mailbox.message_detail.as_ref().unwrap().conversation_id.0,
            "conversation-1"
        );
    }

    #[test]
    fn completion_for_an_inactive_account_is_ignored_but_releases_its_lock() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.accounts.push(account("account-2"));
        let request = mailbox.begin_toggle_star().unwrap();
        mailbox.selected_account = 1;

        assert!(!mailbox.finish_message_action(
            &request.account_id,
            &request.conversation_id,
            &request.action,
            &Ok(()),
        ));
        mailbox.selected_account = 0;
        assert!(!mailbox.selected_message_action_pending());
        assert!(!mailbox.current_thread().unwrap().starred);
    }

    #[test]
    fn inactive_account_refresh_does_not_replace_the_current_view() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.accounts.push(account("account-2"));
        assert!(
            mailbox
                .finish_account_refresh(
                    &MailAccountId("account-1".into()),
                    Some(RefreshFailureKind::Storage),
                )
                .is_some()
        );
        let selected_thread = mailbox.selected_thread.clone();
        let threads = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();

        assert!(
            mailbox
                .finish_account_refresh(
                    &MailAccountId("account-2".into()),
                    Some(RefreshFailureKind::Authentication),
                )
                .is_none()
        );
        assert_eq!(mailbox.selected_thread, selected_thread);
        assert_eq!(
            mailbox
                .threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>(),
            threads
        );
        assert_eq!(
            mailbox.backend_summary(),
            RefreshFailureKind::Storage.status_message()
        );

        mailbox.selected_account = 1;
        assert_eq!(
            mailbox.backend_summary(),
            RefreshFailureKind::Authentication.status_message()
        );
        assert!(
            mailbox
                .finish_account_refresh(&MailAccountId("account-2".into()), None)
                .is_some()
        );
        assert_ne!(
            mailbox.backend_summary(),
            RefreshFailureKind::Authentication.status_message()
        );
        mailbox.selected_account = 0;
        assert_eq!(
            mailbox.backend_summary(),
            RefreshFailureKind::Storage.status_message()
        );
    }

    #[test]
    fn cache_change_reloads_only_the_current_account_view() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.accounts.push(account("account-2"));
        let thread_ids = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();

        assert!(
            mailbox
                .begin_cache_change_reload(&MailAccountId("account-2".into()))
                .is_none()
        );
        assert_eq!(
            mailbox
                .threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>(),
            thread_ids
        );

        assert!(
            mailbox
                .begin_cache_change_reload(&MailAccountId("account-1".into()))
                .is_some()
        );
    }

    #[test]
    fn mailbox_reload_preserves_a_valid_selection_and_rejects_stale_completion() {
        let mut mailbox = mailbox_for_action_tests();
        let first = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();
        let second = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();
        let snapshot = || MailboxContentSnapshot {
            folders: vec![MailFolder {
                id: FolderId("inbox".into()),
                name: "Inbox".into(),
                unread_count: 1,
                kind: FolderKind::Inbox,
            }],
            selected_folder_id: Some(FolderId("inbox".into())),
            conversations: vec![summary(0), summary(9)],
        };

        assert!(!mailbox.finish_mailbox_reload(
            first.request_id,
            &first.account_id,
            Ok(snapshot()),
        ));
        assert!(mailbox.finish_mailbox_reload(
            second.request_id,
            &second.account_id,
            Ok(snapshot()),
        ));
        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-0")
        );
        assert_eq!(
            mailbox
                .message_detail
                .as_ref()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-0")
        );
        assert_eq!(mailbox.threads.len(), 2);
    }

    #[test]
    fn mailbox_reload_clears_a_selection_missing_from_the_new_cache_snapshot() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();

        assert!(mailbox.finish_mailbox_reload(
            request.request_id,
            &request.account_id,
            Ok(MailboxContentSnapshot {
                folders: vec![MailFolder {
                    id: FolderId("archive".into()),
                    name: "Archive".into(),
                    unread_count: 0,
                    kind: FolderKind::Archive,
                }],
                selected_folder_id: Some(FolderId("archive".into())),
                conversations: vec![summary(8)],
            }),
        ));
        assert!(mailbox.selected_thread.is_none());
        assert!(mailbox.message_detail.is_none());
        assert_eq!(mailbox.current_folder().unwrap().id.0, "archive");
    }

    #[test]
    fn failed_mailbox_reload_keeps_cached_content_and_allows_a_retry() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();
        let thread_ids = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();

        assert!(!mailbox.finish_mailbox_reload(
            request.request_id,
            &request.account_id,
            Err("fixture cache failure".into()),
        ));
        assert_eq!(
            mailbox
                .threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>(),
            thread_ids
        );
        assert!(
            mailbox
                .begin_cache_change_reload(&MailAccountId("account-1".into()))
                .is_some()
        );
    }

    #[test]
    fn folder_and_search_changes_invalidate_an_in_flight_mailbox_reload() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        let old_folder_reload = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();
        assert!(mailbox.select_folder(1).is_some());
        assert!(!mailbox.finish_mailbox_reload(
            old_folder_reload.request_id,
            &old_folder_reload.account_id,
            Ok(MailboxContentSnapshot {
                folders: Vec::new(),
                selected_folder_id: None,
                conversations: Vec::new(),
            }),
        ));
        assert_eq!(mailbox.current_folder().unwrap().id.0, "archive");

        mailbox.pending_thread_page = None;
        let old_search_reload = mailbox
            .begin_cache_change_reload(&MailAccountId("account-1".into()))
            .unwrap();
        assert!(mailbox.search("fixture query").is_some());
        assert!(!mailbox.finish_mailbox_reload(
            old_search_reload.request_id,
            &old_search_reload.account_id,
            Ok(MailboxContentSnapshot {
                folders: Vec::new(),
                selected_folder_id: None,
                conversations: Vec::new(),
            }),
        ));
        assert!(
            mailbox
                .begin_cache_change_reload(&MailAccountId("account-1".into()))
                .is_none()
        );
    }

    #[test]
    fn pagination_is_single_flight_and_rejects_a_mismatched_completion() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox
            .begin_thread_page_load()
            .expect("first page request should start");

        assert!(mailbox.begin_thread_page_load().is_none());
        assert!(
            mailbox
                .finish_thread_page_load(
                    request.request_id.wrapping_add(1),
                    &request.account_id,
                    &request.folder_id,
                    request.offset,
                    Ok(Vec::new()),
                )
                .is_none()
        );
        assert!(!mailbox.can_load_more_threads());

        assert!(
            mailbox
                .finish_thread_page_load(
                    request.request_id,
                    &request.account_id,
                    &request.folder_id,
                    request.offset,
                    Ok(Vec::new()),
                )
                .is_some()
        );
        assert!(!mailbox.has_more_threads);
    }

    #[test]
    fn full_page_deduplicates_rows_but_advances_the_backend_cursor() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.begin_thread_page_load().unwrap();
        let batch = (CONVERSATION_PAGE_SIZE - 1..(CONVERSATION_PAGE_SIZE * 2 - 1))
            .map(summary)
            .collect();

        let added = mailbox
            .finish_thread_page_load(
                request.request_id,
                &request.account_id,
                &request.folder_id,
                request.offset,
                Ok(batch),
            )
            .unwrap();

        assert_eq!(added.len(), CONVERSATION_PAGE_SIZE - 1);
        assert_eq!(mailbox.threads.len(), CONVERSATION_PAGE_SIZE * 2 - 1);
        let next = mailbox.begin_thread_page_load().unwrap();
        assert_eq!(next.offset, CONVERSATION_PAGE_SIZE * 2);
    }

    #[test]
    fn pagination_failure_can_retry_the_same_cursor_with_a_new_request_id() {
        let mut mailbox = mailbox_for_pagination_tests();
        let first = mailbox.begin_thread_page_load().unwrap();

        assert!(matches!(
            mailbox.finish_thread_page_load(
                first.request_id,
                &first.account_id,
                &first.folder_id,
                first.offset,
                Err("offline".into()),
            ),
            Some(added) if added.is_empty()
        ));

        let second = mailbox.begin_thread_page_load().unwrap();
        assert_ne!(second.request_id, first.request_id);
        assert_eq!(second.offset, first.offset);
    }

    #[test]
    fn completion_from_a_reset_view_is_ignored() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.begin_thread_page_load().unwrap();
        mailbox.pending_thread_page = None;

        assert!(
            mailbox
                .finish_thread_page_load(
                    request.request_id,
                    &request.account_id,
                    &request.folder_id,
                    request.offset,
                    Ok(vec![summary(99)]),
                )
                .is_none()
        );
        assert!(
            !mailbox
                .threads
                .iter()
                .any(|thread| thread.id.0 == "conversation-99")
        );
    }

    #[test]
    fn late_detail_from_an_old_account_is_ignored() {
        let mut mailbox = mailbox_for_detail_tests();

        let applied = mailbox.finish_message_detail_load(
            0,
            &MailAccountId("account-2".into()),
            &ConversationId("conversation-1".into()),
            Ok(Some(detail("conversation-1"))),
        );

        assert!(!applied);
        assert!(mailbox.message_detail.is_none());
        assert!(mailbox.message_detail_loading);
    }

    #[test]
    fn late_detail_from_a_previous_selection_is_ignored() {
        let mut mailbox = mailbox_for_detail_tests();

        let applied = mailbox.finish_message_detail_load(
            0,
            &MailAccountId("account-1".into()),
            &ConversationId("conversation-old".into()),
            Ok(Some(detail("conversation-old"))),
        );

        assert!(!applied);
        assert!(mailbox.message_detail.is_none());
        assert!(mailbox.message_detail_loading);
    }

    #[test]
    fn changing_selection_retargets_an_in_flight_detail_load() {
        let mut mailbox = mailbox_for_detail_tests();
        mailbox.message_detail = Some(detail("conversation-1"));

        mailbox.select_thread(ConversationId("conversation-2".into()));
        assert!(mailbox.message_detail.is_none());
        assert!(!mailbox.message_detail_loading);

        let request = mailbox
            .begin_message_detail_load()
            .expect("new selection loads");
        assert_eq!(request.conversation_id.0, "conversation-2");
        assert!(mailbox.message_detail_loading);

        assert!(!mailbox.finish_message_detail_load(
            0,
            &request.account_id,
            &ConversationId("conversation-1".into()),
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(mailbox.message_detail.is_none());
        assert!(mailbox.message_detail_loading);

        assert!(mailbox.finish_message_detail_load(
            request.request_id,
            &request.account_id,
            &request.conversation_id,
            Ok(Some(detail("conversation-2"))),
        ));
        assert_eq!(
            mailbox
                .message_detail
                .as_ref()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-2")
        );
    }

    #[test]
    fn returning_to_a_conversation_rejects_its_older_in_flight_result() {
        let mut mailbox = mailbox_for_detail_tests();

        mailbox.select_thread(ConversationId("conversation-2".into()));
        let second = mailbox
            .begin_message_detail_load()
            .expect("second selection should load");
        mailbox.select_thread(ConversationId("conversation-1".into()));
        let current = mailbox
            .begin_message_detail_load()
            .expect("returning selection should start a new generation");

        assert_ne!(second.request_id, current.request_id);
        assert!(!mailbox.finish_message_detail_load(
            0,
            &current.account_id,
            &current.conversation_id,
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(mailbox.message_detail_loading);
        assert!(mailbox.message_detail.is_none());

        assert!(mailbox.finish_message_detail_load(
            current.request_id,
            &current.account_id,
            &current.conversation_id,
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(!mailbox.message_detail_loading);
        assert_eq!(
            mailbox
                .message_detail
                .as_ref()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-1")
        );
    }

    #[test]
    fn current_detail_success_replaces_loading_state() {
        let mut mailbox = mailbox_for_detail_tests();

        let applied = mailbox.finish_message_detail_load(
            0,
            &MailAccountId("account-1".into()),
            &ConversationId("conversation-1".into()),
            Ok(Some(detail("conversation-1"))),
        );

        assert!(applied);
        assert!(!mailbox.message_detail_loading);
        assert_eq!(
            mailbox
                .message_detail
                .as_ref()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-1")
        );
        assert!(mailbox.message_detail_error.is_none());
    }

    #[test]
    fn empty_and_failed_current_results_end_loading_with_an_error() {
        for (result, expected_error) in [
            (Ok(None), "Message unavailable."),
            (Err("backend failed".into()), "backend failed"),
        ] {
            let mut mailbox = mailbox_for_detail_tests();

            assert!(mailbox.finish_message_detail_load(
                0,
                &MailAccountId("account-1".into()),
                &ConversationId("conversation-1".into()),
                result,
            ));
            assert!(!mailbox.message_detail_loading);
            assert!(mailbox.message_detail.is_none());
            assert_eq!(
                mailbox.message_detail_error.as_deref(),
                Some(expected_error)
            );
        }
    }

    #[test]
    fn mismatched_detail_payload_never_replaces_the_requested_message() {
        let mut mailbox = mailbox_for_detail_tests();

        assert!(mailbox.finish_message_detail_load(
            0,
            &MailAccountId("account-1".into()),
            &ConversationId("conversation-1".into()),
            Ok(Some(detail("conversation-2"))),
        ));
        assert!(!mailbox.message_detail_loading);
        assert!(mailbox.message_detail.is_none());
        assert_eq!(
            mailbox.message_detail_error.as_deref(),
            Some("Message unavailable.")
        );
    }
}
