use crate::i18n::{gettext, gettext_f};
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::event::{
    AccountMailboxLoad, MailboxContentSnapshot, RefreshFailureKind, RequestId,
};
use crate::model::mail::{
    AttachmentSource, CONVERSATION_PAGE_SIZE, ConversationId, ConversationSummary, FolderId,
    FolderKind, MailFolder, MailboxMode, MessageAction, MessageDetail, WriteOutcome,
};
use crate::model::settings::AppSettings;

pub(crate) struct MailboxViewModel {
    mailbox_mode: MailboxMode,
    accounts: Vec<MailAccount>,
    selected_account: usize,
    preference_account_id: Option<MailAccountId>,
    folders: Vec<MailFolder>,
    selected_folder: usize,
    threads: Vec<ConversationSummary>,
    pagination: ThreadPagination,
    search: SearchState,
    pending_account_activation: Option<PendingAccountRequest>,
    pending_mailbox_reload: Option<PendingAccountRequest>,
    message_action_requests: std::collections::HashMap<RequestId, MessageActionRequest>,
    refresh_failures: std::collections::HashMap<MailAccountId, RefreshFailureKind>,
    selected_thread: Option<ConversationId>,
    message_detail: MessageDetailState,
    prefer_html_view: bool,
}

struct PendingThreadPage {
    request_id: RequestId,
    offset: usize,
}

enum ThreadPagination {
    Exhausted,
    Ready(usize),
    Loading(PendingThreadPage),
}

fn pagination_after_page(offset: usize, returned: usize) -> ThreadPagination {
    if returned == CONVERSATION_PAGE_SIZE {
        ThreadPagination::Ready(offset.saturating_add(returned))
    } else {
        ThreadPagination::Exhausted
    }
}

struct PendingSearch {
    request_id: RequestId,
    account_id: MailAccountId,
    query: String,
}

enum SearchState {
    Inactive,
    Loading(PendingSearch),
    Results(String),
    Failed { query: String, error: String },
}

struct PendingAccountRequest {
    request_id: RequestId,
    account_id: MailAccountId,
}

struct PendingMessageDetail {
    request_id: RequestId,
    account_id: MailAccountId,
    conversation_id: ConversationId,
}

enum MessageDetailState {
    Empty,
    Loading(PendingMessageDetail),
    Loaded(MessageDetail),
    Failed(String),
}

pub(super) struct AccountActivationRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub conversation_limit: usize,
}

pub(super) struct ThreadPageRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub folder_id: FolderId,
    pub offset: usize,
    pub limit: usize,
}

pub(super) struct MailboxReloadRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub selected_folder_id: Option<FolderId>,
    pub conversation_limit: usize,
}

pub(super) enum MailboxReloadOutcome {
    Applied,
    Failed(String),
}

pub(super) enum ThreadPageOutcome {
    Initial,
    Additional(Vec<ConversationSummary>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountActivationOutcome {
    Live,
    NonLive,
}

pub(super) enum MessageActionOutcome {
    Applied,
    SelectionChanged(MessageDetailRequest),
    Failed(String),
}

pub(super) enum PreviewContent {
    Empty,
    Loading,
    Loaded(MessageDetail),
    Failed(String),
}

pub(super) enum PreviewActions {
    Unavailable,
    Available {
        folder_kind: FolderKind,
        pending: bool,
        has_archive: bool,
        has_trash: bool,
    },
}

pub(super) enum SidebarContent {
    Loading,
    Folders {
        folders: Vec<MailFolder>,
        selected: usize,
    },
}

pub(super) struct ThreadListView {
    pub heading: String,
    pub content: ThreadListContent,
}

pub(super) enum ThreadListContent {
    LoadingMailbox,
    LoadingMessages,
    Searching,
    SearchFailed(String),
    EmptyFolder,
    EmptySearch,
    Messages {
        threads: Vec<ConversationSummary>,
        selected: Option<ConversationId>,
    },
}

pub(super) struct SearchRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub query: String,
}

pub(super) enum ViewReloadRequest {
    Mailbox(MailboxReloadRequest),
    Search(SearchRequest),
}

pub(super) enum SearchTransition {
    Unchanged,
    Cleared,
    Requested(SearchRequest),
}

pub(super) struct MessageDetailRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub conversation_id: ConversationId,
}

#[derive(Clone)]
pub(super) struct MessageActionRequest {
    pub request_id: RequestId,
    pub account_id: MailAccountId,
    pub conversation_id: ConversationId,
    pub action: MessageAction,
}

impl MailboxViewModel {
    pub fn loading_placeholder() -> Self {
        Self {
            mailbox_mode: MailboxMode::Loading,
            accounts: Vec::new(),
            selected_account: 0,
            preference_account_id: None,
            folders: Vec::new(),
            selected_folder: 0,
            threads: Vec::new(),
            pagination: ThreadPagination::Exhausted,
            search: SearchState::Inactive,
            pending_account_activation: None,
            pending_mailbox_reload: None,
            message_action_requests: std::collections::HashMap::new(),
            refresh_failures: std::collections::HashMap::new(),
            selected_thread: None,
            message_detail: MessageDetailState::Empty,
            prefer_html_view: true,
        }
    }

    pub(super) fn is_bootstrap_placeholder(&self) -> bool {
        self.is_loading() && self.accounts.is_empty()
    }

    pub fn is_loading(&self) -> bool {
        self.mailbox_mode == MailboxMode::Loading
    }

    pub(super) fn account_ready(&self) -> bool {
        !self.is_loading() && self.current_account().is_some()
    }

    pub fn accounts(&self) -> &[MailAccount] {
        &self.accounts
    }

    pub fn from_content(
        accounts: Vec<MailAccount>,
        settings: AppSettings,
        mode: MailboxMode,
        content: MailboxContentSnapshot,
    ) -> Self {
        assert!(
            !accounts.is_empty(),
            "only the dedicated loading placeholder may have no account"
        );
        let preferred_account_id = settings.selected_account_id;
        let selected_account = preferred_account_id
            .as_ref()
            .and_then(|selected| accounts.iter().position(|account| account.id == *selected))
            .unwrap_or(0);
        let prefer_html_view = settings.prefer_html_view;
        let MailboxContentSnapshot {
            folders,
            selected_folder_id,
            conversations,
        } = content;
        let selected_folder = selected_folder_id
            .as_ref()
            .and_then(|folder_id| folders.iter().position(|folder| folder.id == *folder_id))
            .unwrap_or(0);
        let pagination = pagination_after_page(0, conversations.len());
        Self {
            mailbox_mode: mode,
            accounts,
            selected_account,
            preference_account_id: preferred_account_id,
            folders,
            selected_folder,
            threads: conversations,
            pagination,
            search: SearchState::Inactive,
            pending_account_activation: None,
            pending_mailbox_reload: None,
            message_action_requests: std::collections::HashMap::new(),
            refresh_failures: std::collections::HashMap::new(),
            selected_thread: None,
            message_detail: MessageDetailState::Empty,
            prefer_html_view,
        }
    }

    pub(crate) fn from_account_catalog(
        accounts: Vec<MailAccount>,
        settings: AppSettings,
        mode: MailboxMode,
    ) -> Self {
        Self::from_content(
            accounts,
            settings,
            mode,
            MailboxContentSnapshot {
                folders: Vec::new(),
                selected_folder_id: None,
                conversations: Vec::new(),
            },
        )
    }

    pub fn status_summary(&self) -> String {
        if self.is_loading() {
            return gettext("Loading mail…");
        }
        if let Some(failure) = self
            .current_account_id()
            .and_then(|account_id| self.refresh_failures.get(&account_id).copied())
        {
            return match failure {
                RefreshFailureKind::Connectivity => gettext("Offline"),
                RefreshFailureKind::Authentication => gettext("Authentication failed"),
                RefreshFailureKind::Storage => gettext("Cache unavailable"),
                RefreshFailureKind::Backend => gettext("Refresh unavailable"),
            };
        }
        match self.mailbox_mode {
            MailboxMode::Loading => gettext("Loading mail…"),
            MailboxMode::NoAccount => gettext("No mail account"),
            MailboxMode::Unavailable => gettext("Mail unavailable"),
            MailboxMode::Live => String::new(),
        }
    }

    pub fn current_account(&self) -> Option<&MailAccount> {
        self.accounts.get(self.selected_account)
    }

    pub fn selected_account_index(&self) -> usize {
        self.selected_account
    }

    pub fn current_account_id(&self) -> Option<MailAccountId> {
        self.current_account().map(|account| account.id.clone())
    }

    pub(crate) fn settings(&self) -> AppSettings {
        AppSettings {
            selected_account_id: self.preference_account_id.clone(),
            prefer_html_view: self.prefer_html_view,
        }
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

    pub(super) fn update_registry_catalog(&mut self, accounts: Vec<MailAccount>) {
        let current_account_id = self
            .current_account_id()
            .expect("a catalog update requires a current account");
        let selected_account = accounts
            .iter()
            .position(|account| account.id == current_account_id)
            .expect("an in-place catalog update must retain the current account");
        self.accounts = accounts;
        self.selected_account = selected_account;
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

    fn current_thread(&self) -> Option<&ConversationSummary> {
        let selected = self.selected_thread.as_ref()?;
        self.threads.iter().find(|thread| thread.id == *selected)
    }

    fn current_thread_folder(&self) -> Option<&MailFolder> {
        let folder_id = &self.current_thread()?.folder_id;
        self.folders.iter().find(|folder| folder.id == *folder_id)
    }

    pub fn message_detail(&self) -> Option<&MessageDetail> {
        match &self.message_detail {
            MessageDetailState::Loaded(detail) => Some(detail),
            _ => None,
        }
    }

    fn message_detail_mut(&mut self) -> Option<&mut MessageDetail> {
        match &mut self.message_detail {
            MessageDetailState::Loaded(detail) => Some(detail),
            _ => None,
        }
    }

    fn message_detail_loading(&self) -> bool {
        matches!(&self.message_detail, MessageDetailState::Loading(_))
    }

    pub(super) fn preview_content(&self) -> PreviewContent {
        match &self.message_detail {
            MessageDetailState::Empty => PreviewContent::Empty,
            MessageDetailState::Loading(_) => PreviewContent::Loading,
            MessageDetailState::Loaded(detail) => PreviewContent::Loaded(detail.clone()),
            MessageDetailState::Failed(error) => PreviewContent::Failed(error.clone()),
        }
    }

    pub(super) fn preview_actions(&self) -> PreviewActions {
        let Some(folder_kind) = self.current_thread_folder().map(|folder| folder.kind) else {
            return PreviewActions::Unavailable;
        };
        if self.message_detail().is_none() {
            return PreviewActions::Unavailable;
        }
        PreviewActions::Available {
            folder_kind,
            pending: self.selected_message_action_pending(),
            has_archive: self
                .folders
                .iter()
                .any(|folder| folder.kind == FolderKind::Archive),
            has_trash: self
                .folders
                .iter()
                .any(|folder| folder.kind == FolderKind::Trash),
        }
    }

    pub(super) fn sidebar_content(&self) -> SidebarContent {
        if self.is_loading() {
            SidebarContent::Loading
        } else {
            SidebarContent::Folders {
                folders: self.folders.clone(),
                selected: self.selected_folder,
            }
        }
    }

    pub(super) fn folder_index(&self, folder_id: &FolderId) -> Option<usize> {
        self.folders.iter().position(|folder| folder.id == *folder_id)
    }

    pub(super) fn folder_selection_changes(&self, index: usize) -> bool {
        index < self.folders.len() && (index != self.selected_folder || self.search_active())
    }

    pub(super) fn thread_list_view(&self) -> ThreadListView {
        let query = self.search_query();
        let (heading, content) = if self.is_loading() {
            (gettext("Mailbox"), ThreadListContent::LoadingMailbox)
        } else if query.is_empty() {
            let heading = self
                .current_folder()
                .map(|folder| folder.name.clone())
                .unwrap_or_else(|| gettext("Mailbox"));
            let content = if self.initial_threads_loading() {
                ThreadListContent::LoadingMessages
            } else if self.threads.is_empty() {
                ThreadListContent::EmptyFolder
            } else {
                self.thread_list_content()
            };
            (heading, content)
        } else if self.search_loading() {
            (gettext("Search"), ThreadListContent::Searching)
        } else if let Some(error) = self.search_error() {
            (
                gettext("Search"),
                ThreadListContent::SearchFailed(error.to_owned()),
            )
        } else {
            let content = if self.threads.is_empty() {
                ThreadListContent::EmptySearch
            } else {
                self.thread_list_content()
            };
            (
                gettext_f("Search results for “{query}”", &[("query", query)]),
                content,
            )
        };
        ThreadListView {
            heading,
            content,
        }
    }

    fn thread_list_content(&self) -> ThreadListContent {
        ThreadListContent::Messages {
            threads: self.threads.clone(),
            selected: self.selected_thread.clone(),
        }
    }

    pub fn select_thread(&mut self, conversation_id: ConversationId) {
        if self.selected_thread.as_ref() == Some(&conversation_id) {
            return;
        }
        self.selected_thread = Some(conversation_id);
        self.clear_message_detail();
    }

    pub(super) fn begin_account_activation(
        &mut self,
        index: usize,
    ) -> Option<AccountActivationRequest> {
        if index >= self.accounts.len() || index == self.selected_account {
            return None;
        }
        let account_id = self.accounts[index].id.clone();
        let request_id = RequestId::next();
        self.pending_account_activation = Some(PendingAccountRequest {
            request_id,
            account_id: account_id.clone(),
        });
        self.selected_account = index;
        self.preference_account_id = Some(account_id.clone());
        self.selected_folder = 0;
        self.search = SearchState::Inactive;
        self.folders.clear();
        self.threads.clear();
        self.pagination = ThreadPagination::Exhausted;
        self.pending_mailbox_reload = None;
        self.clear_message_selection();
        self.mailbox_mode = MailboxMode::Loading;
        Some(AccountActivationRequest {
            request_id,
            account_id,
            conversation_limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub(super) fn begin_initial_activation(&mut self) -> Option<AccountActivationRequest> {
        if !self.is_loading() || self.pending_account_activation.is_some() {
            return None;
        }
        let account_id = self.current_account_id()?;
        let request_id = RequestId::next();
        self.pending_account_activation = Some(PendingAccountRequest {
            request_id,
            account_id: account_id.clone(),
        });
        Some(AccountActivationRequest {
            request_id,
            account_id,
            conversation_limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub(super) fn finish_account_activation(
        &mut self,
        request_id: RequestId,
        load: AccountMailboxLoad,
    ) -> Option<AccountActivationOutcome> {
        let request = self
            .pending_account_activation
            .as_ref()
            .filter(|request| request.request_id == request_id)?;
        if self.current_account_id().as_ref() != Some(&request.account_id) {
            return None;
        }
        let account_id = request.account_id.clone();
        self.pending_account_activation = None;
        let AccountMailboxLoad {
            mode,
            content,
            failure,
        } = load;
        assert_ne!(
            mode,
            MailboxMode::Loading,
            "a completed activation requires a settled mailbox mode"
        );
        self.mailbox_mode = mode;
        self.replace_content(content);
        if let Some(failure) = failure {
            self.refresh_failures.insert(account_id.clone(), failure);
        } else {
            self.refresh_failures.remove(&account_id);
        };
        Some(if self.mailbox_mode == MailboxMode::Live {
            AccountActivationOutcome::Live
        } else {
            AccountActivationOutcome::NonLive
        })
    }

    pub(super) fn select_folder(&mut self, index: usize) -> Option<ThreadPageRequest> {
        if index >= self.folders.len()
            || (index == self.selected_folder && !self.search_active())
        {
            return None;
        }
        self.pending_mailbox_reload = None;
        self.clear_message_selection();
        self.search = SearchState::Inactive;
        self.selected_folder = index;
        self.threads.clear();
        self.pagination = ThreadPagination::Ready(0);
        self.begin_thread_page_load_at(0)
    }

    pub(super) fn search(&mut self, query: &str) -> SearchTransition {
        let query = query.trim().to_string();
        if self.search_query() == query.as_str() {
            return SearchTransition::Unchanged;
        }
        let account_id = if query.is_empty() {
            None
        } else {
            let Some(account) = self.current_account() else {
                return SearchTransition::Unchanged;
            };
            Some(account.id.clone())
        };
        self.clear_message_selection();
        self.pagination = ThreadPagination::Exhausted;
        self.pending_mailbox_reload = None;

        let Some(account_id) = account_id else {
            self.search = SearchState::Inactive;
            self.threads.clear();
            return SearchTransition::Cleared;
        };
        SearchTransition::Requested(self.begin_search_request(account_id, query))
    }

    fn begin_search_request(
        &mut self,
        account_id: MailAccountId,
        query: String,
    ) -> SearchRequest {
        self.threads.clear();
        let request_id = RequestId::next();
        self.search = SearchState::Loading(PendingSearch {
            request_id,
            account_id: account_id.clone(),
            query: query.clone(),
        });
        SearchRequest {
            request_id,
            account_id,
            query,
        }
    }

    pub(super) fn finish_search(
        &mut self,
        request_id: RequestId,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> bool {
        let SearchState::Loading(request) = &self.search else {
            return false;
        };
        if request.request_id != request_id
            || self.current_account_id().as_ref() != Some(&request.account_id)
        {
            return false;
        }
        let query = request.query.clone();
        self.search = match result {
            Ok(results) => {
                self.threads = results;
                SearchState::Results(query)
            }
            Err(error) => {
                self.threads.clear();
                SearchState::Failed {
                    query,
                    error,
                }
            }
        };
        true
    }

    pub(super) fn finish_account_refresh(
        &mut self,
        refreshed_account_id: &MailAccountId,
        failure: Option<RefreshFailureKind>,
    ) -> Option<ViewReloadRequest> {
        if let Some(failure) = failure {
            self.set_refresh_failure(refreshed_account_id.clone(), failure);
        } else {
            self.refresh_failures.remove(refreshed_account_id);
        }
        self.begin_cache_change(refreshed_account_id)
    }

    pub(crate) fn set_refresh_failure(
        &mut self,
        account_id: MailAccountId,
        failure: RefreshFailureKind,
    ) {
        self.refresh_failures.insert(account_id, failure);
    }

    pub(super) fn begin_cache_change(
        &mut self,
        changed_account_id: &MailAccountId,
    ) -> Option<ViewReloadRequest> {
        if self.current_account_id().as_ref() != Some(changed_account_id) || self.is_loading() {
            return None;
        }
        if self.search_active() {
            let query = self.search_query().to_string();
            self.clear_message_selection();
            self.pagination = ThreadPagination::Exhausted;
            self.pending_mailbox_reload = None;
            return Some(ViewReloadRequest::Search(
                self.begin_search_request(changed_account_id.clone(), query),
            ));
        }
        self.begin_mailbox_reload().map(ViewReloadRequest::Mailbox)
    }

    fn begin_mailbox_reload(&mut self) -> Option<MailboxReloadRequest> {
        if self.is_loading() || self.search_active() {
            return None;
        }
        let account_id = self.current_account_id()?;
        let request_id = RequestId::next();
        self.pending_mailbox_reload = Some(PendingAccountRequest {
            request_id,
            account_id: account_id.clone(),
        });
        Some(MailboxReloadRequest {
            request_id,
            account_id,
            selected_folder_id: self.current_folder().map(|folder| folder.id.clone()),
            conversation_limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub(super) fn finish_mailbox_reload(
        &mut self,
        request_id: RequestId,
        result: Result<MailboxContentSnapshot, String>,
    ) -> Option<MailboxReloadOutcome> {
        let request = self
            .pending_mailbox_reload
            .as_ref()
            .filter(|request| request.request_id == request_id)?;
        if self.current_account_id().as_ref() != Some(&request.account_id) {
            return None;
        }
        self.pending_mailbox_reload = None;
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => return Some(MailboxReloadOutcome::Failed(error)),
        };

        let previous_selection = self.selected_thread.clone();
        let previous_detail =
            std::mem::replace(&mut self.message_detail, MessageDetailState::Empty);
        self.replace_content(snapshot);
        self.selected_thread = previous_selection
            .filter(|selected| self.threads.iter().any(|thread| thread.id == *selected));
        if self.selected_thread.is_none() {
            self.clear_message_selection();
        } else {
            self.message_detail = previous_detail;
        }
        Some(MailboxReloadOutcome::Applied)
    }

    fn replace_content(&mut self, content: MailboxContentSnapshot) {
        let MailboxContentSnapshot {
            folders,
            selected_folder_id,
            conversations,
        } = content;
        self.selected_folder = selected_folder_id
            .as_ref()
            .and_then(|folder_id| folders.iter().position(|folder| folder.id == *folder_id))
            .unwrap_or(0);
        self.folders = folders;
        self.pagination = pagination_after_page(0, conversations.len());
        self.threads = conversations;
    }

    fn can_load_more_threads(&self) -> bool {
        matches!(&self.pagination, ThreadPagination::Ready(_))
            && self.pending_mailbox_reload.is_none()
            && !self.search_active()
            && !self.is_loading()
    }

    fn search_loading(&self) -> bool {
        matches!(&self.search, SearchState::Loading(_))
    }

    pub fn search_query(&self) -> &str {
        match &self.search {
            SearchState::Inactive => "",
            SearchState::Loading(request) => &request.query,
            SearchState::Results(query) | SearchState::Failed { query, .. } => query,
        }
    }

    fn search_error(&self) -> Option<&str> {
        match &self.search {
            SearchState::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    fn search_active(&self) -> bool {
        !matches!(&self.search, SearchState::Inactive)
    }

    fn initial_threads_loading(&self) -> bool {
        matches!(
            &self.pagination,
            ThreadPagination::Loading(request) if request.offset == 0 && self.threads.is_empty()
        )
    }

    pub(super) fn begin_thread_page_load(&mut self) -> Option<ThreadPageRequest> {
        if !self.can_load_more_threads() {
            return None;
        }

        let offset = match &self.pagination {
            ThreadPagination::Ready(offset) => *offset,
            _ => return None,
        };
        self.begin_thread_page_load_at(offset)
    }

    fn begin_thread_page_load_at(&mut self, offset: usize) -> Option<ThreadPageRequest> {
        let account_id = self.current_account()?.id.clone();
        let folder_id = self.current_folder()?.id.clone();
        let request_id = RequestId::next();
        self.pagination = ThreadPagination::Loading(PendingThreadPage {
            request_id,
            offset,
        });
        Some(ThreadPageRequest {
            request_id,
            account_id,
            folder_id,
            offset,
            limit: CONVERSATION_PAGE_SIZE,
        })
    }

    pub(super) fn finish_thread_page_load(
        &mut self,
        request_id: RequestId,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> Option<ThreadPageOutcome> {
        let ThreadPagination::Loading(request) = &self.pagination else {
            return None;
        };
        if request.request_id != request_id {
            return None;
        }
        let offset = request.offset;

        let next_batch = match result {
            Ok(batch) => batch,
            Err(_) => {
                self.pagination = if offset == 0 {
                    ThreadPagination::Exhausted
                } else {
                    ThreadPagination::Ready(offset)
                };
                return Some(if offset == 0 {
                    ThreadPageOutcome::Initial
                } else {
                    ThreadPageOutcome::Additional(Vec::new())
                });
            }
        };
        self.pagination = pagination_after_page(offset, next_batch.len());

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
        Some(if offset == 0 {
            ThreadPageOutcome::Initial
        } else {
            ThreadPageOutcome::Additional(added)
        })
    }

    pub(super) fn begin_toggle_star(&mut self) -> Option<MessageActionRequest> {
        let starred = self
            .message_detail()
            .map(|detail| detail.starred)
            .or_else(|| self.current_thread().map(|thread| thread.starred))?;
        self.begin_message_action(MessageAction::SetStarred(!starred))
    }

    pub(super) fn begin_toggle_read(&mut self) -> Option<MessageActionRequest> {
        let unread = self
            .message_detail()
            .map(|detail| detail.unread)
            .or_else(|| self.current_thread().map(|thread| thread.unread_count > 0))?;
        self.begin_message_action(MessageAction::SetRead(unread))
    }

    pub(super) fn begin_archive_selected(&mut self) -> Option<MessageActionRequest> {
        self.begin_move_selected(FolderKind::Archive)
    }

    pub(super) fn begin_trash_selected(&mut self) -> Option<MessageActionRequest> {
        self.begin_move_selected(FolderKind::Trash)
    }

    fn begin_move_selected(&mut self, destination_kind: FolderKind) -> Option<MessageActionRequest> {
        let folder_id = self
            .folders
            .iter()
            .find(|folder| folder.kind == destination_kind)
            .map(|folder| folder.id.clone())?;
        if self.current_thread().is_some_and(|thread| thread.folder_id == folder_id) {
            return None;
        }
        self.begin_message_action(MessageAction::MoveTo(folder_id))
    }

    pub(super) fn selected_message_action_pending(&self) -> bool {
        let Some(account_id) = self.current_account_id() else {
            return false;
        };
        let Some(conversation_id) = self.selected_thread.as_ref() else {
            return false;
        };
        self.message_action_requests
            .values()
            .any(|request| {
                request.account_id == account_id && request.conversation_id == *conversation_id
            })
    }

    fn begin_message_action(&mut self, action: MessageAction) -> Option<MessageActionRequest> {
        let account_id = self.current_account_id()?;
        let conversation_id = self.selected_thread.clone()?;
        if self.message_action_requests.values().any(|request| {
            request.account_id == account_id && request.conversation_id == conversation_id
        }) {
            return None;
        }
        let request_id = RequestId::next();
        let request = MessageActionRequest {
            request_id,
            account_id,
            conversation_id,
            action,
        };
        self.message_action_requests
            .insert(request_id, request.clone());
        Some(request)
    }

    pub(super) fn finish_message_action(
        &mut self,
        request_id: RequestId,
        result: Result<WriteOutcome, String>,
    ) -> Option<MessageActionOutcome> {
        let request = self.message_action_requests.remove(&request_id)?;
        if self.current_account_id().as_ref() != Some(&request.account_id) {
            return None;
        }
        match result {
            Err(error) => return Some(MessageActionOutcome::Failed(error)),
            Ok(WriteOutcome::Unchanged) => return Some(MessageActionOutcome::Applied),
            Ok(WriteOutcome::Applied | WriteOutcome::Queued) => {}
        }

        let conversation_id = request.conversation_id;
        match request.action {
            MessageAction::SetStarred(starred) => {
                if let Some(thread) = self
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == conversation_id)
                {
                    thread.starred = starred;
                }
                if let Some(detail) = self
                    .message_detail_mut()
                    .filter(|detail| detail.conversation_id == conversation_id)
                {
                    detail.starred = starred;
                }
            }
            MessageAction::SetRead(read) => {
                let previous = self
                    .threads
                    .iter()
                    .find(|thread| thread.id == conversation_id)
                    .map(|thread| (thread.folder_id.clone(), thread.unread_count > 0));
                if let Some(thread) = self
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == conversation_id)
                {
                    thread.unread_count = u32::from(!read);
                }
                if let Some(detail) = self
                    .message_detail_mut()
                    .filter(|detail| detail.conversation_id == conversation_id)
                {
                    detail.unread = !read;
                }
                if let Some((folder_id, was_unread)) = previous
                    && let Some(folder) = self
                        .folders
                        .iter_mut()
                        .find(|folder| folder.id == folder_id)
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
            MessageAction::MoveTo(destination_folder_id) => {
                let searching = self.search_active();
                let moved = self
                    .threads
                    .iter()
                    .enumerate()
                    .find(|(_, thread)| thread.id == conversation_id)
                    .map(|(index, thread)| {
                        (index, thread.folder_id.clone(), thread.unread_count)
                    });
                if moved
                    .as_ref()
                    .is_some_and(|(_, folder_id, _)| folder_id == &destination_folder_id)
                {
                    return Some(MessageActionOutcome::Applied);
                }
                let removed = if searching {
                    if let Some(thread) = self
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == conversation_id)
                    {
                        thread.folder_id = destination_folder_id.clone();
                    }
                    false
                } else {
                    let previous_len = self.threads.len();
                    self.threads.retain(|thread| thread.id != conversation_id);
                    self.threads.len() != previous_len
                };
                if let Some((_, source_folder_id, moved_unread)) = moved.as_ref()
                    && source_folder_id != &destination_folder_id
                {
                    if let Some(folder) = self
                        .folders
                        .iter_mut()
                        .find(|folder| &folder.id == source_folder_id)
                    {
                        folder.unread_count = folder.unread_count.saturating_sub(*moved_unread);
                    }
                    if let Some(folder) = self
                        .folders
                        .iter_mut()
                        .find(|folder| folder.id == destination_folder_id)
                    {
                        folder.unread_count = folder.unread_count.saturating_add(*moved_unread);
                    }
                }
                if removed {
                    self.pagination = match &self.pagination {
                        ThreadPagination::Ready(offset) => {
                            ThreadPagination::Ready(offset.saturating_sub(1))
                        }
                        ThreadPagination::Loading(request) => {
                            ThreadPagination::Ready(request.offset.saturating_sub(1))
                        }
                        ThreadPagination::Exhausted => ThreadPagination::Exhausted,
                    };
                }
                if removed && self.selected_thread.as_ref() == Some(&conversation_id) {
                    self.selected_thread = moved.as_ref().and_then(|(index, _, _)| {
                        self.threads
                            .get((*index).min(self.threads.len().saturating_sub(1)))
                            .map(|thread| thread.id.clone())
                    });
                    self.clear_message_detail();
                    if let Some(request) = self.begin_message_detail_load() {
                        return Some(MessageActionOutcome::SelectionChanged(request));
                    }
                }
            }
        }
        Some(MessageActionOutcome::Applied)
    }

    fn clear_message_selection(&mut self) {
        self.selected_thread = None;
        self.clear_message_detail();
    }

    fn clear_message_detail(&mut self) {
        self.message_detail = MessageDetailState::Empty;
    }

    pub(super) fn begin_message_detail_load(&mut self) -> Option<MessageDetailRequest> {
        let account_id = self.current_account()?.id.clone();
        let conversation_id = self.selected_thread.clone()?;
        if self
            .message_detail()
            .is_some_and(|detail| detail.conversation_id == conversation_id)
        {
            return None;
        }
        if self.message_detail_loading() {
            return None;
        }

        let request_id = RequestId::next();
        self.message_detail = MessageDetailState::Loading(PendingMessageDetail {
            request_id,
            account_id: account_id.clone(),
            conversation_id: conversation_id.clone(),
        });
        Some(MessageDetailRequest {
            request_id,
            account_id,
            conversation_id,
        })
    }

    pub(super) fn finish_message_detail_load(
        &mut self,
        request_id: RequestId,
        result: Result<Option<MessageDetail>, String>,
    ) -> bool {
        let MessageDetailState::Loading(request) = &self.message_detail else {
            return false;
        };
        if request.request_id != request_id
            || self.current_account_id().as_ref() != Some(&request.account_id)
            || self.selected_thread.as_ref() != Some(&request.conversation_id)
        {
            return false;
        }
        let conversation_id = request.conversation_id.clone();

        self.message_detail = match result {
            Ok(Some(detail)) if detail.conversation_id == conversation_id => {
                MessageDetailState::Loaded(detail)
            }
            Ok(Some(_)) | Ok(None) => MessageDetailState::Failed(gettext("Message unavailable.")),
            Err(error) => MessageDetailState::Failed(error),
        };
        true
    }

    pub(super) fn current_attachment_source(&self) -> Option<AttachmentSource> {
        let account = self.current_account()?;
        let conversation_id = self.selected_thread.as_ref()?;
        Some(AttachmentSource {
            account_id: account.id.clone(),
            conversation_id: conversation_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mail::AccountMailService;
    use crate::integration::backend::SharedMailBackend;
    use crate::model::account::{SendingIdentity, Signature};
    use crate::model::mail::{FolderKind, MessageId};

    fn account(id: &str) -> MailAccount {
        MailAccount::new(
            MailAccountId(id.into()),
            id.into(),
            SendingIdentity::new(
                format!("{id}@example.com"),
                id.into(),
                None,
                Signature::default(),
            ),
        )
    }

    fn content(
        folders: Vec<MailFolder>,
        conversations: Vec<ConversationSummary>,
    ) -> MailboxContentSnapshot {
        MailboxContentSnapshot {
            selected_folder_id: folders.first().map(|folder| folder.id.clone()),
            folders,
            conversations,
        }
    }

    fn activation(
        mode: MailboxMode,
        folders: Vec<MailFolder>,
        conversations: Vec<ConversationSummary>,
    ) -> AccountMailboxLoad {
        AccountMailboxLoad {
            mode,
            content: content(folders, conversations),
            failure: None,
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
        let mail_service = AccountMailService::new(backend.clone());
        let folders = mail_service.list_folders().unwrap();
        let conversations = folders
            .first()
            .map(|folder| {
                mail_service
                    .list_conversations(&folder.id, 0, CONVERSATION_PAGE_SIZE)
                    .unwrap()
            })
            .unwrap_or_default();
        MailboxViewModel::from_content(
            accounts,
            settings,
            mailbox_mode,
            content(folders, conversations),
        )
    }

    fn mailbox_for_detail_tests() -> MailboxViewModel {
        let mut mailbox = MailboxViewModel::loading_placeholder();
        mailbox.accounts = vec![account("account-1"), account("account-2")];
        mailbox.selected_account = 0;
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox
            .begin_message_detail_load()
            .expect("detail fixture should start one request");
        mailbox
    }

    fn pending_detail_request_id(mailbox: &MailboxViewModel) -> RequestId {
        match &mailbox.message_detail {
            MessageDetailState::Loading(request) => request.request_id,
            _ => panic!("detail fixture should have one pending request"),
        }
    }

    fn start_search(mailbox: &mut MailboxViewModel, query: &str) -> SearchRequest {
        match mailbox.search(query) {
            SearchTransition::Requested(request) => request,
            SearchTransition::Unchanged | SearchTransition::Cleared => {
                panic!("fixture query should start a search")
            }
        }
    }

    fn begin_mailbox_reload(mailbox: &mut MailboxViewModel) -> MailboxReloadRequest {
        match mailbox.begin_cache_change(&MailAccountId("account-1".into())) {
            Some(ViewReloadRequest::Mailbox(request)) => request,
            Some(ViewReloadRequest::Search(_)) | None => {
                panic!("folder view cache change should start a mailbox reload")
            }
        }
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
            date_unix_secs: 0,
            starred: false,
            unread: false,
            attachments: Vec::new(),
            body: crate::model::mail::MessageBody::Empty,
        }
    }

    fn summary(index: usize) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(format!("conversation-{index}")),
            folder_id: FolderId("inbox".into()),
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
        let mut mailbox = MailboxViewModel::loading_placeholder();
        mailbox.accounts = vec![account("account-1")];
        mailbox.folders = vec![MailFolder {
            id: FolderId("inbox".into()),
            name: "Inbox".into(),
            unread_count: 0,
            kind: FolderKind::Inbox,
        }];
        mailbox.threads = (0..CONVERSATION_PAGE_SIZE).map(summary).collect();
        mailbox.pagination = ThreadPagination::Ready(CONVERSATION_PAGE_SIZE);
        mailbox.mailbox_mode = MailboxMode::Live;
        mailbox
    }

    fn mailbox_for_action_tests() -> MailboxViewModel {
        let mut mailbox = mailbox_for_pagination_tests();
        mailbox.selected_thread = Some(ConversationId("conversation-0".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-0"));
        mailbox
    }

    #[test]
    fn no_eligible_eds_account_opens_a_small_stub_mailbox() {
        let mailbox = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            crate::integration::stub::mail_backend(),
            MailboxMode::NoAccount,
        );

        assert_eq!(mailbox.accounts.len(), 1);
        assert_eq!(mailbox.mailbox_mode, MailboxMode::NoAccount);
        assert_eq!(mailbox.threads.len(), 2);
        let mut thread_ids = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();
        thread_ids.sort_by(|left, right| left.0.cmp(&right.0));
        thread_ids.dedup();
        assert_eq!(thread_ids.len(), mailbox.threads.len());
        assert_eq!(
            mailbox.current_account().unwrap().default_identity().address,
            mailbox.current_account().unwrap().primary_identity().address
        );
    }

    #[test]
    fn initial_and_later_account_activation_share_the_same_request_state() {
        let mut mailbox = MailboxViewModel::from_content(
            vec![account("account-1")],
            AppSettings::default(),
            MailboxMode::Loading,
            content(Vec::new(), Vec::new()),
        );

        let request = mailbox
            .begin_initial_activation()
            .expect("a loading catalog starts its initial activation");
        assert_eq!(request.account_id.0, "account-1");
        assert!(mailbox.begin_initial_activation().is_none());
        assert_eq!(
            mailbox.finish_account_activation(
                request.request_id,
                activation(MailboxMode::Live, Vec::new(), Vec::new()),
            ),
            Some(AccountActivationOutcome::Live)
        );
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
            let mailbox = MailboxViewModel::from_content(
                vec![account("account-1"), account("account-2")],
                AppSettings {
                    selected_account_id: Some(MailAccountId("account-2".into())),
                    ..AppSettings::default()
                },
                MailboxMode::Live,
                content(folders.clone(), conversations.clone()),
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
            assert_eq!(
                mailbox.can_load_more_threads(),
                conversation_count == CONVERSATION_PAGE_SIZE
            );
        }
    }

    #[test]
    fn unrelated_registry_changes_update_the_catalog_without_resetting_the_current_view() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.accounts.push(account("account-2"));
        let visible_threads = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();
        let replacement = MailboxViewModel::from_content(
            vec![account("account-2"), account("account-1"), account("account-3")],
            AppSettings {
                selected_account_id: Some(MailAccountId("account-1".into())),
                ..AppSettings::default()
            },
            MailboxMode::Live,
            content(Vec::new(), Vec::new()),
        );

        mailbox.update_registry_catalog(replacement.accounts.clone());
        assert_eq!(
            mailbox.current_account_id(),
            Some(MailAccountId("account-1".into()))
        );
        assert_eq!(mailbox.selected_account, 1);
        assert_eq!(mailbox.accounts.len(), 3);
        assert_eq!(
            mailbox
                .threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>(),
            visible_threads
        );
        assert_eq!(
            mailbox
                .message_detail()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-0")
        );

        mailbox.update_registry_catalog(replacement.accounts);
    }

    #[test]
    fn registry_completion_cannot_revert_a_newer_account_selection() {
        let mut mailbox = mailbox_for_pagination_tests();
        mailbox.accounts.push(account("account-2"));
        let activation = mailbox
            .begin_account_activation(1)
            .expect("the newer account selection should start activation");
        let replacement = MailboxViewModel::from_content(
            vec![account("account-1"), account("account-2")],
            AppSettings {
                selected_account_id: Some(MailAccountId("account-1".into())),
                ..AppSettings::default()
            },
            MailboxMode::Live,
            content(Vec::new(), Vec::new()),
        );

        mailbox.update_registry_catalog(replacement.accounts);
        assert_eq!(mailbox.current_account_id(), Some(activation.account_id));
        assert!(mailbox.pending_account_activation.is_some());
        assert_eq!(mailbox.mailbox_mode, MailboxMode::Loading);
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
                crate::integration::stub::mail_backend(),
                MailboxMode::NoAccount,
            );

            assert_eq!(mailbox.prefer_html_view, prefer_html_view);

            mailbox.set_prefer_html_view(!prefer_html_view);
            assert_eq!(mailbox.prefer_html_view, !prefer_html_view);
            mailbox.set_prefer_html_view(prefer_html_view);
            assert_eq!(mailbox.prefer_html_view, prefer_html_view);
        }
    }

    #[test]
    fn refresh_failure_state_is_scoped_and_success_clears_it() {
        let mut mailbox = mailbox_for_pagination_tests();
        let account_id = mailbox.current_account_id().unwrap();

        for failure in [
            RefreshFailureKind::Connectivity,
            RefreshFailureKind::Authentication,
            RefreshFailureKind::Storage,
            RefreshFailureKind::Backend,
        ] {
            mailbox.set_refresh_failure(account_id.clone(), failure);
            assert_eq!(mailbox.refresh_failures.get(&account_id), Some(&failure));
        }

        start_search(&mut mailbox, "active search");
        assert!(matches!(
            mailbox.finish_account_refresh(&account_id, None),
            Some(ViewReloadRequest::Search(request)) if request.query == "active search"
        ));
        assert!(!mailbox.refresh_failures.contains_key(&account_id));
        assert!(matches!(
            mailbox
                .finish_account_refresh(
                    &account_id,
                    Some(RefreshFailureKind::Connectivity),
                ),
            Some(ViewReloadRequest::Search(request)) if request.query == "active search"
        ));
        assert_eq!(
            mailbox.refresh_failures.get(&account_id),
            Some(&RefreshFailureKind::Connectivity)
        );
    }

    #[test]
    fn unavailable_stub_keeps_a_sending_identity_for_ui_exploration() {
        let mailbox = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            crate::integration::stub::mail_backend(),
            MailboxMode::Unavailable,
        );
        let account_id = mailbox.current_account_id().unwrap();

        assert_eq!(
            mailbox.current_account().unwrap().default_identity().address,
            mailbox.current_account().unwrap().primary_identity().address
        );
        assert_eq!(account_id.0, "local-stub");
    }

    #[test]
    fn only_the_empty_loading_model_is_a_bootstrap_placeholder() {
        let backend = crate::integration::stub::mail_backend();
        let placeholder = MailboxViewModel::loading_placeholder();
        assert!(placeholder.is_bootstrap_placeholder());

        let no_account = loaded_mailbox(
            Vec::new(),
            AppSettings::default(),
            backend.clone(),
            MailboxMode::NoAccount,
        );
        assert!(!no_account.is_bootstrap_placeholder());

        let unavailable = MailboxViewModel::from_content(
            vec![account("account-1")],
            AppSettings::default(),
            MailboxMode::Unavailable,
            content(Vec::new(), Vec::new()),
        );
        assert!(!unavailable.is_loading());
        assert_eq!(unavailable.mailbox_mode, MailboxMode::Unavailable);
        assert!(!unavailable.is_bootstrap_placeholder());

        let mut activating = MailboxViewModel::loading_placeholder();
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

        assert!(mailbox.finish_account_activation(
            request.request_id.wrapping_add(1),
            activation(MailboxMode::Live, Vec::new(), Vec::new()),
        ).is_none());

        let folders = vec![MailFolder {
            id: FolderId("inbox".into()),
            name: "Inbox".into(),
            unread_count: 1,
            kind: FolderKind::Inbox,
        }];
        assert_eq!(
            mailbox.finish_account_activation(
                request.request_id,
                activation(MailboxMode::Live, folders.clone(), vec![summary(42)]),
            ),
            Some(AccountActivationOutcome::Live)
        );
        assert!(!mailbox.is_loading());
        assert_eq!(mailbox.folders.len(), 1);
        assert_eq!(mailbox.folders[0].id.0, "inbox");
        assert_eq!(mailbox.folders[0].unread_count, 1);
        assert_eq!(mailbox.threads[0].id.0, "conversation-42");
        assert!(!mailbox.can_load_more_threads());
    }

    #[test]
    fn activated_account_keeps_its_mode_when_the_initial_cache_read_fails() {
        let mut mailbox = mailbox_for_detail_tests();
        let request = mailbox.begin_account_activation(1).unwrap();

        assert_eq!(
            mailbox.finish_account_activation(
                request.request_id,
                AccountMailboxLoad {
                    mode: MailboxMode::Live,
                    content: content(Vec::new(), Vec::new()),
                    failure: Some(RefreshFailureKind::Storage),
                },
            ),
            Some(AccountActivationOutcome::Live)
        );

        assert_eq!(mailbox.mailbox_mode, MailboxMode::Live);
        assert_eq!(
            mailbox.refresh_failures.get(&request.account_id),
            Some(&RefreshFailureKind::Storage)
        );
    }

    #[test]
    fn newer_account_activation_supersedes_an_older_completion() {
        let mut mailbox = mailbox_for_detail_tests();
        let second_account = mailbox.begin_account_activation(1).unwrap();
        let first_account = mailbox.begin_account_activation(0).unwrap();

        assert!(mailbox.finish_account_activation(
            second_account.request_id,
            activation(MailboxMode::Live, Vec::new(), vec![summary(20)]),
        ).is_none());
        assert_eq!(
            mailbox.finish_account_activation(
                first_account.request_id,
                activation(
                    MailboxMode::Unavailable,
                    Vec::new(),
                    vec![summary(10)],
                ),
            ),
            Some(AccountActivationOutcome::NonLive)
        );
        assert_eq!(mailbox.current_account_id().unwrap().0, "account-1");
        assert_eq!(mailbox.threads[0].id.0, "conversation-10");
        assert_eq!(mailbox.mailbox_mode, MailboxMode::Unavailable);
    }

    #[test]
    fn failed_account_activation_ends_loading_without_exposing_backend_detail() {
        let mut mailbox = mailbox_for_detail_tests();
        let request = mailbox.begin_account_activation(1).unwrap();
        mailbox.set_refresh_failure(
            request.account_id.clone(),
            RefreshFailureKind::Connectivity,
        );

        assert_eq!(
            mailbox.finish_account_activation(
                request.request_id,
                AccountMailboxLoad {
                    mode: MailboxMode::Unavailable,
                    content: content(Vec::new(), Vec::new()),
                    failure: Some(RefreshFailureKind::Connectivity),
                },
            ),
            Some(AccountActivationOutcome::NonLive)
        );
        assert!(!mailbox.is_loading());
        assert_eq!(mailbox.mailbox_mode, MailboxMode::Unavailable);
        assert_eq!(
            mailbox.refresh_failures.get(&request.account_id),
            Some(&RefreshFailureKind::Connectivity)
        );
        assert!(mailbox.search_error().is_none());
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
        mailbox.message_detail = MessageDetailState::Failed("old detail failure".into());

        let request = mailbox
            .select_folder(1)
            .expect("a different folder should start a scoped load");

        assert_eq!(mailbox.selected_folder, 1);
        assert_eq!(request.folder_id.0, "archive");
        assert_eq!(request.offset, 0);
        assert!(mailbox.initial_threads_loading());
        assert!(mailbox.threads.is_empty());
        assert!(mailbox.selected_thread.is_none());
        assert!(mailbox.message_detail().is_none());
        assert!(!mailbox.message_detail_loading());
        assert!(matches!(mailbox.preview_content(), PreviewContent::Empty));

        let added = mailbox
            .finish_thread_page_load(
                request.request_id,
                Ok(vec![summary(7)]),
            )
            .expect("the current folder load should be accepted");
        assert!(matches!(added, ThreadPageOutcome::Initial));
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
        assert!(mailbox.message_detail().is_some());
    }

    #[test]
    fn selecting_a_folder_while_searching_exits_search_and_loads_that_folder() {
        let mut mailbox = mailbox_for_action_tests();
        start_search(&mut mailbox, "needle");

        let request = mailbox
            .select_folder(0)
            .expect("the selected folder should replace search results");

        assert!(mailbox.search_query().is_empty());
        assert_eq!(request.folder_id, FolderId("inbox".into()));
        assert_eq!(request.offset, 0);
        assert!(mailbox.initial_threads_loading());
        assert!(mailbox.selected_thread.is_none());
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

        let request = start_search(&mut mailbox, "  needle  ");

        assert_eq!(request.account_id.0, "account-1");
        assert_eq!(request.query, "needle");
        assert_eq!(mailbox.search_query(), "needle");
        assert!(mailbox.threads.is_empty());
        assert!(!mailbox.can_load_more_threads());
    }

    #[test]
    fn request_ids_are_not_reused_when_the_mailbox_model_is_replaced() {
        let mut old_mailbox = mailbox_for_pagination_tests();
        let old = start_search(&mut old_mailbox, "same query");
        let mut replacement = mailbox_for_pagination_tests();
        let current = start_search(&mut replacement, "same query");

        assert_ne!(old.request_id, current.request_id);
        assert!(!replacement.finish_search(
            old.request_id,
            Ok(vec![summary(99)]),
        ));
        assert!(replacement.threads.is_empty());
    }

    #[test]
    fn stale_search_completion_cannot_replace_a_newer_query() {
        let mut mailbox = mailbox_for_pagination_tests();
        let first = start_search(&mut mailbox, "first");
        let second = start_search(&mut mailbox, "second");

        assert!(!mailbox.finish_search(
            first.request_id,
            Ok(vec![summary(90)]),
        ));
        assert!(mailbox.threads.is_empty());
        assert!(mailbox.finish_search(
            second.request_id,
            Ok(vec![summary(91)]),
        ));
        assert_eq!(mailbox.threads[0].id.0, "conversation-91");
    }

    #[test]
    fn failed_search_is_scoped_and_exposes_a_safe_error() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = start_search(&mut mailbox, "needle");
        let failure = "opaque failure".to_string();

        assert!(mailbox.finish_search(
            request.request_id,
            Err(failure.clone()),
        ));
        assert_eq!(mailbox.search_query(), "needle");
        assert_eq!(mailbox.search_error(), Some(failure.as_str()));
        assert!(mailbox.threads.is_empty());
    }

    #[test]
    fn clearing_search_invalidates_pending_completion() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = start_search(&mut mailbox, "needle");

        assert!(matches!(mailbox.search(""), SearchTransition::Cleared));
        assert!(mailbox.search_query().is_empty());
        assert!(mailbox.threads.is_empty());
        assert!(
            mailbox
                .begin_cache_change(&request.account_id)
                .is_some()
        );
        assert!(mailbox.begin_thread_page_load().is_none());
        assert!(!mailbox.finish_search(
            request.request_id,
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

        assert!(matches!(
            mailbox.finish_message_action(
                request.request_id,
                Ok(WriteOutcome::Applied),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert!(mailbox.current_thread().unwrap().starred);
        assert!(mailbox.message_detail().unwrap().starred);
    }

    #[test]
    fn old_model_action_completion_cannot_release_a_replacement_action() {
        let mut old_mailbox = mailbox_for_action_tests();
        let old = old_mailbox.begin_toggle_star().unwrap();
        let mut replacement = mailbox_for_action_tests();
        let current = replacement.begin_toggle_star().unwrap();

        assert_ne!(old.request_id, current.request_id);
        assert!(
            replacement
                .finish_message_action(
                    old.request_id,
                    Err("stale failure".into()),
                )
                .is_none()
        );
        assert!(replacement.selected_message_action_pending());
        assert!(!replacement.current_thread().unwrap().starred);
    }

    #[test]
    fn unchanged_actions_release_the_request_without_projecting_a_mutation() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-trash".into()),
            name: "Trash".into(),
            unread_count: 0,
            kind: FolderKind::Trash,
        });
        let original_thread_count = mailbox.threads.len();
        let original_starred = mailbox.current_thread().unwrap().starred;
        let original_selection = mailbox.selected_thread.clone();

        let star = mailbox.begin_toggle_star().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                star.request_id,
                Ok(WriteOutcome::Unchanged),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert_eq!(mailbox.current_thread().unwrap().starred, original_starred);

        let move_request = mailbox.begin_trash_selected().unwrap();
        assert_eq!(
            move_request.action,
            MessageAction::MoveTo(FolderId("provider-trash".into()))
        );
        assert!(matches!(
            mailbox.finish_message_action(
                move_request.request_id,
                Ok(WriteOutcome::Unchanged),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert_eq!(mailbox.threads.len(), original_thread_count);
        assert_eq!(mailbox.selected_thread, original_selection);
    }

    #[test]
    fn failed_message_action_releases_pending_state_without_mutation() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox.begin_toggle_read().unwrap();

        assert!(matches!(
            mailbox.finish_message_action(
                request.request_id,
                Err("offline".into()),
            ),
            Some(MessageActionOutcome::Failed(error)) if error == "offline"
        ));
        assert!(!mailbox.selected_message_action_pending());
        assert!(!mailbox.message_detail().unwrap().unread);
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 0);
        assert!(mailbox.begin_toggle_read().is_some());
    }

    #[test]
    fn read_completion_keeps_message_thread_and_folder_counts_consistent() {
        let mut mailbox = mailbox_for_action_tests();
        let mark_unread = mailbox.begin_toggle_read().unwrap();

        assert!(matches!(
            mailbox.finish_message_action(
                mark_unread.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 1);
        assert_eq!(mailbox.current_folder().unwrap().unread_count, 1);
        assert!(mailbox.message_detail().unwrap().unread);

        let mark_read = mailbox.begin_toggle_read().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                mark_read.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert_eq!(mailbox.current_thread().unwrap().unread_count, 0);
        assert_eq!(mailbox.current_folder().unwrap().unread_count, 0);
        assert!(!mailbox.message_detail().unwrap().unread);
    }

    #[test]
    fn search_result_read_completion_updates_its_modeled_folder() {
        let mut mailbox = mailbox_for_pagination_tests();
        let search = start_search(&mut mailbox, "sender");
        assert!(mailbox.finish_search(search.request_id, Ok(vec![summary(99)])));
        mailbox.select_thread(ConversationId("conversation-99".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-99"));

        let mark_unread = mailbox.begin_toggle_read().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                mark_unread.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));

        assert_eq!(mailbox.current_thread().unwrap().unread_count, 1);
        assert!(mailbox.message_detail().unwrap().unread);
        assert_eq!(mailbox.current_folder().unwrap().unread_count, 1);
    }

    #[test]
    fn folder_move_completion_updates_known_source_and_destination_counts() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 2,
            kind: FolderKind::Archive,
        });
        let mark_unread = mailbox.begin_toggle_read().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                mark_unread.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));

        let archive = mailbox.begin_archive_selected().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                archive.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::SelectionChanged(_))
        ));

        assert_eq!(mailbox.folders[0].unread_count, 0);
        assert_eq!(mailbox.folders[1].unread_count, 3);
    }

    #[test]
    fn moving_the_selected_folder_row_loads_its_nearest_remaining_neighbor() {
        fn mailbox_with_archive() -> MailboxViewModel {
            let mut mailbox = mailbox_for_action_tests();
            mailbox.folders.push(MailFolder {
                id: FolderId("provider-archive".into()),
                name: "Archive".into(),
                unread_count: 0,
                kind: FolderKind::Archive,
            });
            mailbox
        }

        let mut middle = mailbox_with_archive();
        middle.select_thread(ConversationId("conversation-5".into()));
        middle.message_detail = MessageDetailState::Loaded(detail("conversation-5"));
        let action = middle.begin_archive_selected().unwrap();
        let Some(MessageActionOutcome::SelectionChanged(request)) = middle
            .finish_message_action(action.request_id, Ok(WriteOutcome::Queued))
        else {
            panic!("moving the selected row should request its neighbor");
        };
        assert_eq!(request.conversation_id.0, "conversation-6");
        assert_eq!(middle.selected_thread, Some(request.conversation_id));
        assert!(middle.message_detail_loading());

        let mut final_row = mailbox_with_archive();
        let final_index = CONVERSATION_PAGE_SIZE - 1;
        let final_id = ConversationId(format!("conversation-{final_index}"));
        final_row.select_thread(final_id.clone());
        final_row.message_detail = MessageDetailState::Loaded(detail(&final_id.0));
        let action = final_row.begin_archive_selected().unwrap();
        let Some(MessageActionOutcome::SelectionChanged(request)) = final_row
            .finish_message_action(action.request_id, Ok(WriteOutcome::Queued))
        else {
            panic!("moving the final row should request its previous neighbor");
        };
        assert_eq!(
            request.conversation_id.0,
            format!("conversation-{}", final_index - 1)
        );
        assert_eq!(final_row.selected_thread, Some(request.conversation_id));
        assert!(final_row.message_detail_loading());
    }

    #[test]
    fn search_result_move_completion_preserves_the_result_with_its_new_origin() {
        let mut mailbox = mailbox_for_pagination_tests();
        mailbox.folders[0].unread_count = 1;
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 2,
            kind: FolderKind::Archive,
        });
        let search = start_search(&mut mailbox, "sender");
        let mut result = summary(99);
        result.unread_count = 1;
        assert!(mailbox.finish_search(search.request_id, Ok(vec![result])));
        mailbox.select_thread(ConversationId("conversation-99".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-99"));

        let archive = mailbox.begin_archive_selected().unwrap();
        assert!(matches!(
            mailbox.finish_message_action(
                archive.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));

        assert_eq!(mailbox.threads.len(), 1);
        assert_eq!(mailbox.threads[0].folder_id.0, "provider-archive");
        assert_eq!(mailbox.folders[0].unread_count, 0);
        assert_eq!(mailbox.folders[1].unread_count, 3);
        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-99")
        );
        assert_eq!(
            mailbox.message_detail().map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-99")
        );
    }

    #[test]
    fn search_result_actions_use_its_origin_instead_of_the_pre_search_folder() {
        let mut mailbox = mailbox_for_pagination_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        mailbox.selected_folder = 1;
        let search = start_search(&mut mailbox, "sender");
        assert!(mailbox.finish_search(search.request_id, Ok(vec![summary(99)])));
        mailbox.select_thread(ConversationId("conversation-99".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-99"));

        assert!(matches!(
            mailbox.preview_actions(),
            PreviewActions::Available {
                folder_kind: FolderKind::Inbox,
                ..
            }
        ));
        assert!(mailbox.begin_archive_selected().is_some());
    }

    #[test]
    fn folder_view_does_not_offer_a_move_to_its_current_destination() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders[0].kind = FolderKind::Archive;

        assert!(mailbox.begin_archive_selected().is_none());
        assert!(!mailbox.selected_message_action_pending());
    }

    #[test]
    fn move_completion_does_not_reapply_an_already_reloaded_projection() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        let archive = mailbox.begin_archive_selected().unwrap();
        mailbox.threads[0].folder_id = FolderId("provider-archive".into());

        assert!(matches!(
            mailbox.finish_message_action(
                archive.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));

        assert_eq!(mailbox.threads.len(), CONVERSATION_PAGE_SIZE);
        assert_eq!(
            mailbox.current_thread().unwrap().folder_id,
            FolderId("provider-archive".into())
        );
    }

    #[test]
    fn late_action_updates_its_row_without_overwriting_newer_detail() {
        let mut mailbox = mailbox_for_action_tests();
        let request = mailbox.begin_toggle_star().unwrap();
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-1"));

        assert!(matches!(
            mailbox.finish_message_action(
                request.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
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
            mailbox.message_detail().unwrap().conversation_id.0,
            "conversation-1"
        );
        assert!(!mailbox.message_detail().unwrap().starred);
    }

    #[test]
    fn late_move_does_not_replace_a_newer_selection() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        let request = mailbox.begin_archive_selected().unwrap();
        assert_eq!(
            request.action,
            MessageAction::MoveTo(FolderId("provider-archive".into()))
        );
        mailbox.selected_thread = Some(ConversationId("conversation-1".into()));
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-1"));

        assert!(matches!(
            mailbox.finish_message_action(
                request.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::Applied)
        ));
        assert!(
            !mailbox
                .threads
                .iter()
                .any(|thread| thread.id == request.conversation_id)
        );
        assert!(matches!(
            &mailbox.pagination,
            ThreadPagination::Ready(offset) if *offset == CONVERSATION_PAGE_SIZE - 1
        ));
        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-1")
        );
        assert_eq!(
            mailbox.message_detail().unwrap().conversation_id.0,
            "conversation-1"
        );
    }

    #[test]
    fn move_actions_are_absent_without_a_cached_destination_folder() {
        let mut mailbox = mailbox_for_action_tests();

        assert!(mailbox.begin_archive_selected().is_none());
        assert!(mailbox.begin_trash_selected().is_none());
        assert!(!mailbox.selected_message_action_pending());
    }

    #[test]
    fn completion_for_a_background_account_is_ignored_but_releases_its_lock() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.accounts.push(account("account-2"));
        let request = mailbox.begin_toggle_star().unwrap();
        mailbox.selected_account = 1;

        assert!(
            mailbox
                .finish_message_action(
                    request.request_id,
                    Ok(WriteOutcome::Queued),
                )
                .is_none()
        );
        mailbox.selected_account = 0;
        assert!(!mailbox.selected_message_action_pending());
        assert!(!mailbox.current_thread().unwrap().starred);
    }

    #[test]
    fn background_account_refresh_does_not_replace_the_current_view() {
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
            mailbox
                .refresh_failures
                .get(&MailAccountId("account-1".into())),
            Some(&RefreshFailureKind::Storage)
        );

        mailbox.selected_account = 1;
        assert_eq!(
            mailbox
                .refresh_failures
                .get(&MailAccountId("account-2".into())),
            Some(&RefreshFailureKind::Authentication)
        );
        assert!(
            mailbox
                .finish_account_refresh(&MailAccountId("account-2".into()), None)
                .is_some()
        );
        assert!(!mailbox
            .refresh_failures
            .contains_key(&MailAccountId("account-2".into())));
        mailbox.selected_account = 0;
        assert_eq!(
            mailbox
                .refresh_failures
                .get(&MailAccountId("account-1".into())),
            Some(&RefreshFailureKind::Storage)
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
                .begin_cache_change(&MailAccountId("account-2".into()))
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
                .begin_cache_change(&MailAccountId("account-1".into()))
                .is_some()
        );
    }

    #[test]
    fn mailbox_reload_preserves_a_valid_selection_and_rejects_stale_completion() {
        let mut mailbox = mailbox_for_action_tests();
        let first = begin_mailbox_reload(&mut mailbox);
        let second = begin_mailbox_reload(&mut mailbox);
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

        assert!(
            mailbox
                .finish_mailbox_reload(first.request_id, Ok(snapshot()))
                .is_none()
        );
        assert!(matches!(
            mailbox.finish_mailbox_reload(second.request_id, Ok(snapshot())),
            Some(MailboxReloadOutcome::Applied)
        ));
        assert_eq!(
            mailbox.selected_thread.as_ref().map(|id| id.0.as_str()),
            Some("conversation-0")
        );
        assert_eq!(
            mailbox
                .message_detail()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-0")
        );
        assert_eq!(mailbox.threads.len(), 2);
    }

    #[test]
    fn mailbox_reload_preserves_an_in_flight_detail_for_a_retained_selection() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.clear_message_detail();
        let detail_request = mailbox.begin_message_detail_load().unwrap();
        let reload = begin_mailbox_reload(&mut mailbox);
        let folders = mailbox.folders.clone();

        assert!(matches!(
            mailbox.finish_mailbox_reload(
                reload.request_id,
                Ok(MailboxContentSnapshot {
                    folders,
                    selected_folder_id: Some(FolderId("inbox".into())),
                    conversations: vec![summary(0)],
                }),
            ),
            Some(MailboxReloadOutcome::Applied)
        ));
        assert!(mailbox.message_detail_loading());
        assert!(mailbox.finish_message_detail_load(
            detail_request.request_id,
            Ok(Some(detail("conversation-0"))),
        ));
        assert_eq!(
            mailbox.message_detail().map(|detail| &detail.conversation_id),
            Some(&detail_request.conversation_id)
        );
    }

    #[test]
    fn mailbox_reload_clears_a_selection_missing_from_the_new_cache_snapshot() {
        let mut mailbox = mailbox_for_action_tests();
        let request = begin_mailbox_reload(&mut mailbox);

        assert!(matches!(
            mailbox.finish_mailbox_reload(
                request.request_id,
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
            ),
            Some(MailboxReloadOutcome::Applied)
        ));
        assert!(mailbox.selected_thread.is_none());
        assert!(mailbox.message_detail().is_none());
        assert_eq!(mailbox.current_folder().unwrap().id.0, "archive");
    }

    #[test]
    fn failed_mailbox_reload_reports_failure_preserves_content_and_allows_retry() {
        let mut mailbox = mailbox_for_action_tests();
        let request = begin_mailbox_reload(&mut mailbox);
        let thread_ids = mailbox
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();

        assert!(matches!(
            mailbox.finish_mailbox_reload(
                request.request_id,
                Err("fixture cache failure".into()),
            ),
            Some(MailboxReloadOutcome::Failed(error))
                if error == "fixture cache failure"
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
                .begin_cache_change(&MailAccountId("account-1".into()))
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
        let old_folder_reload = begin_mailbox_reload(&mut mailbox);
        assert!(mailbox.select_folder(1).is_some());
        assert!(
            mailbox
                .finish_mailbox_reload(
                    old_folder_reload.request_id,
                    Ok(MailboxContentSnapshot {
                        folders: Vec::new(),
                        selected_folder_id: None,
                        conversations: Vec::new(),
                    }),
                )
                .is_none()
        );
        assert_eq!(mailbox.current_folder().unwrap().id.0, "archive");

        mailbox.pagination = ThreadPagination::Exhausted;
        let old_search_reload = begin_mailbox_reload(&mut mailbox);
        let search = start_search(&mut mailbox, "fixture query");
        assert!(
            mailbox
                .finish_mailbox_reload(
                    old_search_reload.request_id,
                    Ok(MailboxContentSnapshot {
                        folders: Vec::new(),
                        selected_folder_id: None,
                        conversations: Vec::new(),
                    }),
                )
                .is_none()
        );
        let restarted = match mailbox.begin_cache_change(&MailAccountId("account-1".into())) {
            Some(ViewReloadRequest::Search(request)) => request,
            Some(ViewReloadRequest::Mailbox(_)) | None => {
                panic!("search view cache change should restart its query")
            }
        };
        assert_eq!(restarted.query, "fixture query");
        assert!(!mailbox.finish_search(search.request_id, Ok(vec![summary(9)])));
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
                    Ok(Vec::new()),
                )
                .is_none()
        );
        assert!(!mailbox.can_load_more_threads());

        assert!(
            mailbox
                .finish_thread_page_load(
                    request.request_id,
                    Ok(Vec::new()),
                )
                .is_some()
        );
        assert!(!mailbox.can_load_more_threads());
    }

    #[test]
    fn full_page_deduplicates_rows_but_advances_the_backend_cursor() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.begin_thread_page_load().unwrap();
        let batch = (CONVERSATION_PAGE_SIZE - 1..(CONVERSATION_PAGE_SIZE * 2 - 1))
            .map(summary)
            .collect();

        let outcome = mailbox
            .finish_thread_page_load(
                request.request_id,
                Ok(batch),
            )
            .unwrap();

        assert!(matches!(
            outcome,
            ThreadPageOutcome::Additional(added)
                if added.len() == CONVERSATION_PAGE_SIZE - 1
        ));
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
                Err("offline".into()),
            ),
            Some(ThreadPageOutcome::Additional(added)) if added.is_empty()
        ));

        let second = mailbox.begin_thread_page_load().unwrap();
        assert_ne!(second.request_id, first.request_id);
        assert_eq!(second.offset, first.offset);
    }

    #[test]
    fn moving_a_visible_row_invalidates_an_in_flight_page_and_repairs_its_cursor() {
        let mut mailbox = mailbox_for_action_tests();
        mailbox.folders.push(MailFolder {
            id: FolderId("provider-archive".into()),
            name: "Archive".into(),
            unread_count: 0,
            kind: FolderKind::Archive,
        });
        let page = mailbox.begin_thread_page_load().unwrap();
        let action = mailbox.begin_archive_selected().unwrap();

        assert!(matches!(
            mailbox.finish_message_action(
                action.request_id,
                Ok(WriteOutcome::Queued),
            ),
            Some(MessageActionOutcome::SelectionChanged(_))
        ));
        assert!(mailbox
            .finish_thread_page_load(
                page.request_id,
                Ok(vec![summary(CONVERSATION_PAGE_SIZE)]),
            )
            .is_none());
        assert_eq!(
            mailbox.begin_thread_page_load().unwrap().offset,
            CONVERSATION_PAGE_SIZE - 1
        );
    }

    #[test]
    fn completion_from_a_reset_view_is_ignored() {
        let mut mailbox = mailbox_for_pagination_tests();
        let request = mailbox.begin_thread_page_load().unwrap();
        mailbox.pagination = ThreadPagination::Exhausted;

        assert!(
            mailbox
                .finish_thread_page_load(
                    request.request_id,
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
        let request_id = pending_detail_request_id(&mailbox);
        mailbox.selected_account = 1;

        let applied = mailbox.finish_message_detail_load(
            request_id,
            Ok(Some(detail("conversation-1"))),
        );

        assert!(!applied);
        assert!(mailbox.message_detail().is_none());
        assert!(mailbox.message_detail_loading());
    }

    #[test]
    fn late_detail_from_a_previous_selection_is_ignored() {
        let mut mailbox = mailbox_for_detail_tests();
        let request_id = pending_detail_request_id(&mailbox);
        mailbox.selected_thread = Some(ConversationId("conversation-old".into()));

        let applied = mailbox.finish_message_detail_load(
            request_id,
            Ok(Some(detail("conversation-old"))),
        );

        assert!(!applied);
        assert!(mailbox.message_detail().is_none());
        assert!(mailbox.message_detail_loading());
    }

    #[test]
    fn changing_selection_retargets_an_in_flight_detail_load() {
        let mut mailbox = mailbox_for_detail_tests();
        let old_request_id = pending_detail_request_id(&mailbox);
        mailbox.message_detail = MessageDetailState::Loaded(detail("conversation-1"));

        mailbox.select_thread(ConversationId("conversation-2".into()));
        assert!(mailbox.message_detail().is_none());
        assert!(!mailbox.message_detail_loading());

        let request = mailbox
            .begin_message_detail_load()
            .expect("new selection loads");
        assert_eq!(request.conversation_id.0, "conversation-2");
        assert!(mailbox.message_detail_loading());

        assert!(!mailbox.finish_message_detail_load(
            old_request_id,
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(mailbox.message_detail().is_none());
        assert!(mailbox.message_detail_loading());

        assert!(mailbox.finish_message_detail_load(
            request.request_id,
            Ok(Some(detail("conversation-2"))),
        ));
        assert_eq!(
            mailbox
                .message_detail()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-2")
        );
    }

    #[test]
    fn returning_to_a_conversation_rejects_its_older_in_flight_result() {
        let mut mailbox = mailbox_for_detail_tests();
        let first_request_id = pending_detail_request_id(&mailbox);

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
            first_request_id,
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(mailbox.message_detail_loading());
        assert!(mailbox.message_detail().is_none());

        assert!(mailbox.finish_message_detail_load(
            current.request_id,
            Ok(Some(detail("conversation-1"))),
        ));
        assert!(!mailbox.message_detail_loading());
        assert_eq!(
            mailbox
                .message_detail()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-1")
        );
    }

    #[test]
    fn current_detail_success_replaces_loading_state() {
        let mut mailbox = mailbox_for_detail_tests();
        let request_id = pending_detail_request_id(&mailbox);

        let applied = mailbox.finish_message_detail_load(
            request_id,
            Ok(Some(detail("conversation-1"))),
        );

        assert!(applied);
        assert!(!mailbox.message_detail_loading());
        assert_eq!(
            mailbox
                .message_detail()
                .map(|detail| detail.conversation_id.0.as_str()),
            Some("conversation-1")
        );
        assert!(matches!(
            mailbox.preview_content(),
            PreviewContent::Loaded(_)
        ));
    }

    #[test]
    fn empty_and_failed_current_results_end_loading_with_an_error() {
        for result in [Ok(None), Err("backend failed".into())] {
            let mut mailbox = mailbox_for_detail_tests();
            let request_id = pending_detail_request_id(&mailbox);

            assert!(mailbox.finish_message_detail_load(
                request_id,
                result,
            ));
            assert!(!mailbox.message_detail_loading());
            assert!(mailbox.message_detail().is_none());
            assert!(matches!(
                mailbox.preview_content(),
                PreviewContent::Failed(_)
            ));
        }
    }

    #[test]
    fn mismatched_detail_payload_never_replaces_the_requested_message() {
        let mut mailbox = mailbox_for_detail_tests();
        let request_id = pending_detail_request_id(&mailbox);

        assert!(mailbox.finish_message_detail_load(
            request_id,
            Ok(Some(detail("conversation-2"))),
        ));
        assert!(!mailbox.message_detail_loading());
        assert!(mailbox.message_detail().is_none());
        assert!(matches!(
            mailbox.preview_content(),
            PreviewContent::Failed(_)
        ));
    }
}
