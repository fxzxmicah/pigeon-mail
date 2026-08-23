use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak, mpsc};

use crate::core::mail::MailService;
use crate::integration::backend::EdsAccountBinding;
use crate::integration::camel::ChangeMonitor;
use crate::model::account::MailAccountId;
use crate::model::event::{
    AccountMailboxSnapshot, AttachmentDisposition, CacheEvent, MailboxContentSnapshot,
    MessageAction,
};
use crate::model::mail::{ConversationId, ConversationSummary, DraftMessage, FolderId};

const NOTIFICATION_SNAPSHOT_LIMIT: usize = 256;

#[derive(Clone)]
pub struct CacheManager {
    state: Arc<CacheManagerState>,
}

struct CacheManagerState {
    sender: mpsc::Sender<CacheEvent>,
    receiver: Mutex<mpsc::Receiver<CacheEvent>>,
    fully_active_account: Mutex<Option<MailAccountId>>,
    account_refreshes: Mutex<HashMap<MailAccountId, Option<AccountRefreshJob>>>,
    account_searches: LatestJobQueue<MailAccountId, SearchJob>,
    message_details: LatestJobQueue<MailAccountId, MessageDetailJob>,
    notification_baselines:
        Mutex<HashMap<MailAccountId, HashMap<FolderId, HashMap<ConversationId, i64>>>>,
    remote_change_accounts: Mutex<HashSet<MailAccountId>>,
    change_monitor: Mutex<ChangeMonitorState>,
}

struct AccountRefresh {
    manager: CacheManager,
    account_id: Option<MailAccountId>,
}

#[derive(Clone)]
struct SearchJob {
    service: MailService,
    request_id: u64,
    account_id: MailAccountId,
    query: String,
}

#[derive(Clone)]
struct MessageDetailJob {
    service: MailService,
    request_id: u64,
    account_id: MailAccountId,
    conversation_id: ConversationId,
}

#[derive(Clone)]
struct AccountRefreshJob {
    service: MailService,
}

struct FolderNotificationSnapshot {
    folder_id: FolderId,
    folder_name: String,
    conversations: Vec<ConversationSummary>,
}

struct NotificationSnapshot {
    folder_ids: HashSet<FolderId>,
    folders: Vec<FolderNotificationSnapshot>,
}

#[derive(Debug, PartialEq, Eq)]
struct FolderNotification {
    folder_id: FolderId,
    folder_name: String,
    count: usize,
}

struct LatestJobQueue<K, J> {
    entries: Mutex<HashMap<K, Option<J>>>,
}

#[derive(Clone)]
struct RemoteChangePublisher {
    state: Weak<CacheManagerState>,
}

#[derive(Default)]
struct ChangeMonitorState {
    generation: u64,
    account_id: Option<MailAccountId>,
    starting: bool,
    monitor: Option<ChangeMonitor>,
}

impl<K, J> LatestJobQueue<K, J>
where
    K: Eq + Hash,
{
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn begin(&self, key: K, job: J) -> bool {
        let mut entries = self.entries.lock().expect("latest job queue lock poisoned");
        if let Some(pending) = entries.get_mut(&key) {
            *pending = Some(job);
            return false;
        }
        entries.insert(key, None);
        true
    }

    fn finish(&self, key: &K) -> Option<J> {
        let mut entries = self.entries.lock().expect("latest job queue lock poisoned");
        let next = entries.get_mut(key)?.take();
        if next.is_none() {
            entries.remove(key);
        }
        next
    }

    fn cancel_pending(&self, key: &K) {
        if let Some(pending) = self
            .entries
            .lock()
            .expect("latest job queue lock poisoned")
            .get_mut(key)
        {
            *pending = None;
        }
    }
}

impl CacheManager {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            state: Arc::new(CacheManagerState {
                sender,
                receiver: Mutex::new(receiver),
                fully_active_account: Mutex::new(None),
                account_refreshes: Mutex::new(HashMap::new()),
                account_searches: LatestJobQueue::new(),
                message_details: LatestJobQueue::new(),
                notification_baselines: Mutex::new(HashMap::new()),
                remote_change_accounts: Mutex::new(HashSet::new()),
                change_monitor: Mutex::new(ChangeMonitorState::default()),
            }),
        }
    }

    fn publish(&self, event: CacheEvent) {
        let _ = self.state.sender.send(event);
    }

    pub fn drain(&self) -> Vec<CacheEvent> {
        let receiver = self
            .state
            .receiver
            .lock()
            .expect("cache event receiver lock poisoned");
        receiver.try_iter().collect()
    }

    pub fn request_account_activation(
        &self,
        mail_service: MailService,
        request_id: u64,
        account_id: MailAccountId,
        conversation_limit: usize,
    ) {
        self.set_fully_active_account(Some(account_id.clone()));
        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(mail_service.activate_account(&account_id))
                .and_then(|mode| {
                    futures::executor::block_on(async {
                        let folders = mail_service.list_folders(&account_id).await?;
                        let conversations = if let Some(folder) = folders.first() {
                            mail_service
                                .list_conversations(&account_id, &folder.id, 0, conversation_limit)
                                .await?
                        } else {
                            Vec::new()
                        };
                        Ok::<_, anyhow::Error>(AccountMailboxSnapshot {
                            mode,
                            folders,
                            conversations,
                        })
                    })
                });
            let result = result.map_err(|error| {
                crate::logging::report_failure("mail-account-activation", &error);
                "Account unavailable.".to_string()
            });
            manager.publish(CacheEvent::account_activated(
                request_id, account_id, result,
            ));
        });
    }

    pub fn request_account_refresh(&self, account_id: MailAccountId, service: MailService) -> bool {
        let job = AccountRefreshJob { service };
        let refresh = {
            let mut refreshes = self
                .state
                .account_refreshes
                .lock()
                .expect("account refresh lock poisoned");
            if let Some(pending) = refreshes.get_mut(&account_id) {
                *pending = Some(job);
                return false;
            }
            refreshes.insert(account_id.clone(), None);
            AccountRefresh {
                manager: self.clone(),
                account_id: Some(account_id.clone()),
            }
        };
        let manager = self.clone();
        std::thread::spawn(move || {
            let failure =
                match futures::executor::block_on(job.service.refresh_account(&account_id)) {
                    Ok(()) if manager.is_fully_active_account(&account_id) => {
                        match futures::executor::block_on(load_notification_snapshot(
                            &job.service,
                            &account_id,
                        )) {
                            Ok(snapshot) => {
                                for notification in manager.observe_folders(&account_id, snapshot) {
                                    manager.publish(CacheEvent::new_mail(
                                        account_id.clone(),
                                        notification.folder_id,
                                        notification.folder_name,
                                        notification.count,
                                    ));
                                }
                            }
                            Err(error) => {
                                crate::logging::report_deferred("new-mail-snapshot", &error);
                            }
                        }
                        None
                    }
                    Ok(()) => None,
                    Err(error) => {
                        crate::logging::report_deferred("mail-refresh", &error);
                        Some(crate::failure::classify_failure(&error))
                    }
                };
            refresh.complete(failure);
        });
        true
    }

    pub fn set_fully_active_account(&self, account_id: Option<MailAccountId>) {
        *self
            .state
            .fully_active_account
            .lock()
            .expect("active account lock poisoned") = account_id.clone();
        let mut monitor = self
            .state
            .change_monitor
            .lock()
            .expect("change monitor lock poisoned");
        if monitor.account_id != account_id {
            monitor.generation = monitor.generation.wrapping_add(1);
            monitor.account_id = account_id;
            monitor.starting = false;
            monitor.monitor = None;
            self.state
                .remote_change_accounts
                .lock()
                .expect("remote change lock poisoned")
                .clear();
        }
    }

    pub fn start_account_change_monitor(
        &self,
        account_id: MailAccountId,
        binding: EdsAccountBinding,
    ) {
        if !self.is_fully_active_account(&account_id) {
            return;
        }
        let generation = {
            let mut state = self
                .state
                .change_monitor
                .lock()
                .expect("change monitor lock poisoned");
            if state.account_id.as_ref() == Some(&account_id)
                && (state.starting || state.monitor.is_some())
            {
                return;
            }
            state.generation = state.generation.wrapping_add(1);
            state.account_id = Some(account_id.clone());
            state.starting = true;
            state.monitor = None;
            state.generation
        };
        let change_publisher = self.remote_change_publisher();
        let manager = self.clone();
        std::thread::spawn(move || {
            let callback_account_id = account_id.clone();
            let result = ChangeMonitor::open(&binding, move || {
                change_publisher.publish(&callback_account_id);
            });
            let mut state = manager
                .state
                .change_monitor
                .lock()
                .expect("change monitor lock poisoned");
            if state.generation != generation || state.account_id.as_ref() != Some(&account_id) {
                return;
            }
            state.starting = false;
            state.monitor = match result {
                Ok(monitor) => Some(monitor),
                Err(error) => {
                    tracing::debug!(
                        target: "pigeon::eds",
                        "active account change monitor unavailable"
                    );
                    #[cfg(debug_assertions)]
                    tracing::debug!(
                        target: "pigeon::development::eds",
                        error = %error,
                        "change monitor setup failed"
                    );
                    drop(error);
                    None
                }
            };
        });
    }

    pub fn reset_account_change_monitor(&self, account_id: &MailAccountId) {
        let mut state = self
            .state
            .change_monitor
            .lock()
            .expect("change monitor lock poisoned");
        if state.account_id.as_ref() != Some(account_id) {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        state.starting = false;
        state.monitor = None;
    }

    fn remote_change_publisher(&self) -> RemoteChangePublisher {
        RemoteChangePublisher {
            state: Arc::downgrade(&self.state),
        }
    }

    pub fn acknowledge_remote_change(&self, account_id: &MailAccountId) {
        self.state
            .remote_change_accounts
            .lock()
            .expect("remote change lock poisoned")
            .remove(account_id);
    }

    fn is_fully_active_account(&self, account_id: &MailAccountId) -> bool {
        self.state
            .fully_active_account
            .lock()
            .expect("active account lock poisoned")
            .as_ref()
            == Some(account_id)
    }

    pub fn request_message_detail(
        &self,
        mail_service: MailService,
        request_id: u64,
        account_id: MailAccountId,
        conversation_id: ConversationId,
    ) {
        let job = MessageDetailJob {
            service: mail_service,
            request_id,
            account_id,
            conversation_id,
        };
        if !self.begin_message_detail(job.clone()) {
            return;
        }
        self.spawn_message_detail(job);
    }

    pub fn cancel_pending_message_detail(&self, account_id: &MailAccountId) {
        self.state.message_details.cancel_pending(account_id);
    }

    fn begin_message_detail(&self, job: MessageDetailJob) -> bool {
        self.state
            .message_details
            .begin(job.account_id.clone(), job)
    }

    fn spawn_message_detail(&self, job: MessageDetailJob) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(
                job.service
                    .message_detail(&job.account_id, &job.conversation_id),
            )
            .map_err(|error| {
                crate::logging::report_failure("message-detail-load", &error);
                "Message unavailable.".to_string()
            });
            manager.publish(CacheEvent::message_detail(
                job.request_id,
                job.account_id.clone(),
                job.conversation_id.clone(),
                result,
            ));
            if let Some(next) = manager.finish_message_detail(&job.account_id) {
                manager.spawn_message_detail(next);
            }
        });
    }

    fn finish_message_detail(&self, account_id: &MailAccountId) -> Option<MessageDetailJob> {
        self.state.message_details.finish(account_id)
    }

    pub fn request_thread_page(
        &self,
        mail_service: MailService,
        request_id: u64,
        account_id: MailAccountId,
        folder_id: FolderId,
        offset: usize,
        limit: usize,
    ) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(mail_service.list_conversations(
                &account_id,
                &folder_id,
                offset,
                limit,
            ))
            .map_err(|error| {
                crate::logging::report_failure("message-page-load", &error);
                "Messages unavailable.".to_string()
            });
            manager.publish(CacheEvent::thread_page(
                request_id, account_id, folder_id, offset, result,
            ));
        });
    }

    pub fn request_mailbox_reload(
        &self,
        mail_service: MailService,
        request_id: u64,
        account_id: MailAccountId,
        selected_folder_id: Option<FolderId>,
        conversation_limit: usize,
    ) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(async {
                let folders = mail_service.list_folders(&account_id).await?;
                let selected_folder_id = selected_folder_id
                    .filter(|selected| folders.iter().any(|folder| folder.id == *selected))
                    .or_else(|| folders.first().map(|folder| folder.id.clone()));
                let conversations = if let Some(folder_id) = selected_folder_id.as_ref() {
                    mail_service
                        .list_conversations(&account_id, folder_id, 0, conversation_limit)
                        .await?
                } else {
                    Vec::new()
                };
                Ok::<_, anyhow::Error>(MailboxContentSnapshot {
                    folders,
                    selected_folder_id,
                    conversations,
                })
            })
            .map_err(|error| {
                crate::logging::report_failure("mailbox-cache-reload", &error);
                "Cache reload failed.".to_string()
            });
            manager.publish(CacheEvent::mailbox_reloaded(request_id, account_id, result));
        });
    }

    pub fn request_search(
        &self,
        service: MailService,
        request_id: u64,
        account_id: MailAccountId,
        query: String,
    ) -> bool {
        let job = SearchJob {
            service,
            request_id,
            account_id,
            query,
        };
        if !self.begin_search(job.clone()) {
            return false;
        }
        self.spawn_search(job);
        true
    }

    pub fn cancel_pending_search(&self, account_id: &MailAccountId) {
        self.state.account_searches.cancel_pending(account_id);
    }

    fn begin_search(&self, job: SearchJob) -> bool {
        self.state
            .account_searches
            .begin(job.account_id.clone(), job)
    }

    fn spawn_search(&self, job: SearchJob) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let result =
                futures::executor::block_on(job.service.search(&job.account_id, &job.query))
                    .map_err(|error| {
                        crate::logging::report_failure("mail-search", &error);
                        "Search unavailable.".to_string()
                    });
            manager.publish(CacheEvent::search(
                job.request_id,
                job.account_id.clone(),
                job.query,
                result,
            ));
            if let Some(next) = manager.finish_search(&job.account_id) {
                manager.spawn_search(next);
            }
        });
    }

    fn finish_search(&self, account_id: &MailAccountId) -> Option<SearchJob> {
        self.state.account_searches.finish(account_id)
    }

    pub fn request_attachment(
        &self,
        request: Option<(MailService, MailAccountId, ConversationId)>,
        disposition: AttachmentDisposition,
        display_name: String,
        source_uri: String,
    ) -> bool {
        let Some((mail_service, account_id, conversation_id)) = request else {
            let Ok(uri) = resolve_attachment_uri(None, source_uri) else {
                return false;
            };
            self.publish(CacheEvent::attachment_prepared(
                disposition,
                display_name,
                Ok(uri),
            ));
            return true;
        };

        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(mail_service.open_attachment(
                &account_id,
                &conversation_id,
                &source_uri,
            ))
            .map_err(|error| {
                crate::logging::report_failure("attachment-prepare", &error);
                "Attachment unavailable.".to_string()
            })
            .and_then(|prepared_uri| resolve_attachment_uri(prepared_uri, source_uri));
            manager.publish(CacheEvent::attachment_prepared(
                disposition,
                display_name,
                result,
            ));
        });
        true
    }

    pub fn request_message_action(
        &self,
        service: MailService,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        action: MessageAction,
    ) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let result = futures::executor::block_on(async {
                match &action {
                    MessageAction::SetStarred(starred) => {
                        service
                            .set_starred(&account_id, &conversation_id, *starred)
                            .await
                    }
                    MessageAction::SetRead(read) => {
                        service.set_read(&account_id, &conversation_id, *read).await
                    }
                    MessageAction::MoveTo(folder_id) => {
                        service
                            .move_to_folder(&account_id, &conversation_id, folder_id)
                            .await
                    }
                }
            })
            .map_err(|error| {
                crate::logging::report_failure("mail-action-cache-commit", &error);
                "Change not saved.".to_string()
            });
            let locally_committed = result.is_ok() && service.eds_binding(&account_id).is_some();
            manager.publish(CacheEvent::message_action(
                account_id.clone(),
                conversation_id,
                action,
                result,
            ));
            if locally_committed {
                manager.request_account_refresh(account_id, service);
            }
        });
    }

    pub fn request_save_draft(&self, service: MailService, draft: DraftMessage) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let account_id = draft.account_id.clone();
            let result = futures::executor::block_on(service.save_draft(&draft)).map_err(|error| {
                crate::logging::report_failure("draft-cache-save", &error);
                "Draft not saved.".to_string()
            });
            let locally_saved = matches!(&result, Ok(Some(_)));
            manager.publish(CacheEvent::draft_saved(result));
            if locally_saved {
                manager.publish(CacheEvent::mailbox_changed(account_id.clone()));
                manager.request_account_refresh(account_id, service);
            }
        });
    }

    pub fn request_send_draft(&self, service: MailService, draft: DraftMessage) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let account_id = draft.account_id.clone();
            let result = futures::executor::block_on(service.send_draft(&draft)).map_err(|error| {
                crate::logging::report_failure("message-send", &error);
                "Message not sent.".to_string()
            });
            let locally_queued = matches!(&result, Ok(true));
            manager.publish(CacheEvent::send_completed(result.map(|_| ())));
            if locally_queued {
                manager.publish(CacheEvent::mailbox_changed(account_id.clone()));
                manager.request_account_refresh(account_id, service);
            }
        });
    }

    #[cfg(test)]
    fn begin_account_refresh(&self, account_id: &MailAccountId) -> Option<AccountRefresh> {
        let mut refreshes = self
            .state
            .account_refreshes
            .lock()
            .expect("account refresh lock poisoned");
        if refreshes.contains_key(account_id) {
            return None;
        }
        refreshes.insert(account_id.clone(), None);
        Some(AccountRefresh {
            manager: self.clone(),
            account_id: Some(account_id.clone()),
        })
    }

    fn finish_account_refresh(&self, account_id: &MailAccountId) -> Option<AccountRefreshJob> {
        self.state
            .account_refreshes
            .lock()
            .expect("account refresh lock poisoned")
            .remove(account_id)
            .flatten()
    }

    fn observe_folders(
        &self,
        account_id: &MailAccountId,
        snapshot: NotificationSnapshot,
    ) -> Vec<FolderNotification> {
        let mut baselines = self
            .state
            .notification_baselines
            .lock()
            .expect("notification baseline lock poisoned");
        let mut current = baselines.remove(account_id).unwrap_or_default();
        current.retain(|folder_id, _| snapshot.folder_ids.contains(folder_id));
        let mut notifications = Vec::new();

        for folder in snapshot.folders {
            let folder_baseline = folder
                .conversations
                .iter()
                .map(|summary| (summary.id.clone(), summary.last_updated_unix_ms))
                .collect::<HashMap<_, _>>();
            if let Some(previous_folder) = current.get(&folder.folder_id) {
                let count = folder
                    .conversations
                    .iter()
                    .filter(|summary| {
                        summary.unread_count > 0
                            && previous_folder
                                .get(&summary.id)
                                .is_none_or(|known| summary.last_updated_unix_ms > *known)
                    })
                    .count();
                if count > 0 {
                    notifications.push(FolderNotification {
                        folder_id: folder.folder_id.clone(),
                        folder_name: folder.folder_name,
                        count,
                    });
                }
            }
            current.insert(folder.folder_id, folder_baseline);
        }

        baselines.insert(account_id.clone(), current);
        notifications
    }
}

fn resolve_attachment_uri(
    prepared_uri: Option<String>,
    source_uri: String,
) -> Result<String, String> {
    prepared_uri
        .or_else(|| {
            (source_uri.contains("://") && !source_uri.starts_with("pigeon-eds-attachment:"))
                .then_some(source_uri)
        })
        .ok_or_else(|| "Attachment unavailable.".to_string())
}

impl RemoteChangePublisher {
    fn publish(&self, account_id: &MailAccountId) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        if state
            .fully_active_account
            .lock()
            .expect("active account lock poisoned")
            .as_ref()
            != Some(account_id)
        {
            return;
        }
        let inserted = state
            .remote_change_accounts
            .lock()
            .expect("remote change lock poisoned")
            .insert(account_id.clone());
        if inserted {
            let _ = state
                .sender
                .send(CacheEvent::remote_changed(account_id.clone()));
        }
    }
}

async fn load_notification_snapshot(
    mail_service: &MailService,
    account_id: &MailAccountId,
) -> anyhow::Result<NotificationSnapshot> {
    let folders = mail_service.list_folders(account_id).await?;
    let folder_ids = folders.iter().map(|folder| folder.id.clone()).collect();
    let mut snapshots = Vec::with_capacity(folders.len());
    for folder in folders {
        let conversations = match mail_service
            .list_conversations(account_id, &folder.id, 0, NOTIFICATION_SNAPSHOT_LIMIT)
            .await
        {
            Ok(conversations) => conversations,
            Err(error) => {
                crate::logging::report_deferred("new-mail-folder-snapshot", &error);
                continue;
            }
        };
        snapshots.push(FolderNotificationSnapshot {
            folder_id: folder.id,
            folder_name: folder.name,
            conversations,
        });
    }
    Ok(NotificationSnapshot {
        folder_ids,
        folders: snapshots,
    })
}

impl AccountRefresh {
    fn complete(mut self, failure: Option<crate::model::event::RefreshFailureKind>) {
        let Some(account_id) = self.account_id.take() else {
            return;
        };
        let pending = self.manager.finish_account_refresh(&account_id);
        self.manager
            .publish(CacheEvent::account_refresh(account_id.clone(), failure));
        if let Some(job) = pending {
            self.manager
                .request_account_refresh(account_id, job.service);
        }
    }
}

impl Drop for AccountRefresh {
    fn drop(&mut self) {
        if let Some(account_id) = self.account_id.take() {
            self.manager.finish_account_refresh(&account_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, updated: i64, unread: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(id.into()),
            subject: String::new(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: unread,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: updated,
            preview: String::new(),
        }
    }

    fn folder(
        id: &str,
        name: &str,
        conversations: Vec<ConversationSummary>,
    ) -> FolderNotificationSnapshot {
        FolderNotificationSnapshot {
            folder_id: FolderId(id.into()),
            folder_name: name.into(),
            conversations,
        }
    }

    fn snapshot(folders: Vec<FolderNotificationSnapshot>) -> NotificationSnapshot {
        NotificationSnapshot {
            folder_ids: folders
                .iter()
                .map(|folder| folder.folder_id.clone())
                .collect(),
            folders,
        }
    }

    #[test]
    fn account_refresh_is_single_flight_until_completion() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        let refresh = manager
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        assert!(manager.begin_account_refresh(&account_id).is_none());

        refresh.complete(None);

        assert!(manager.begin_account_refresh(&account_id).is_some());
    }

    #[test]
    fn folder_observation_seeds_silently_then_reports_each_source_folder() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());

        assert!(
            manager
                .observe_folders(
                    &account_id,
                    snapshot(vec![
                        folder(
                            "inbox",
                            "Inbox",
                            vec![summary("shared", 10, 1), summary("flag-only", 10, 0)],
                        ),
                        folder("custom", "Receipts", vec![summary("shared", 10, 1)]),
                        folder("archive", "Archive", Vec::new()),
                    ]),
                )
                .is_empty()
        );

        assert_eq!(
            manager.observe_folders(
                &account_id,
                snapshot(vec![
                    folder(
                        "inbox",
                        "Inbox",
                        vec![
                            summary("shared", 10, 0),
                            summary("flag-only", 10, 1),
                            summary("read", 20, 0),
                        ],
                    ),
                    folder("custom", "Receipts", vec![summary("shared", 20, 1)],),
                    folder(
                        "archive",
                        "Archive",
                        vec![summary("new", 30, 1), summary("second", 30, 1),],
                    ),
                ]),
            ),
            vec![
                FolderNotification {
                    folder_id: FolderId("custom".into()),
                    folder_name: "Receipts".into(),
                    count: 1,
                },
                FolderNotification {
                    folder_id: FolderId("archive".into()),
                    folder_name: "Archive".into(),
                    count: 2,
                },
            ]
        );
        assert!(
            manager
                .observe_folders(
                    &account_id,
                    snapshot(vec![
                        folder(
                            "inbox",
                            "Inbox",
                            vec![summary("shared", 10, 0), summary("flag-only", 10, 1)],
                        ),
                        folder("custom", "Receipts", vec![summary("shared", 20, 1)],),
                        folder(
                            "archive",
                            "Archive",
                            vec![summary("new", 30, 1), summary("second", 30, 1),],
                        ),
                    ]),
                )
                .is_empty()
        );
    }

    #[test]
    fn folder_observation_is_scoped_per_account_and_forgets_absent_folders() {
        let manager = CacheManager::new();

        assert!(
            manager
                .observe_folders(
                    &MailAccountId("account-1".into()),
                    snapshot(vec![folder(
                        "custom",
                        "Custom",
                        vec![summary("shared", 10, 1)],
                    )]),
                )
                .is_empty()
        );
        assert!(
            manager
                .observe_folders(
                    &MailAccountId("account-2".into()),
                    snapshot(vec![folder(
                        "custom",
                        "Custom",
                        vec![summary("shared", 20, 1)],
                    )]),
                )
                .is_empty()
        );
        assert!(
            manager
                .observe_folders(&MailAccountId("account-1".into()), snapshot(Vec::new()),)
                .is_empty()
        );
        assert!(
            manager
                .observe_folders(
                    &MailAccountId("account-1".into()),
                    snapshot(vec![folder(
                        "custom",
                        "Custom",
                        vec![summary("shared", 30, 1)],
                    )]),
                )
                .is_empty()
        );
        assert_eq!(
            manager.observe_folders(
                &MailAccountId("account-2".into()),
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("shared", 30, 1)],
                )]),
            ),
            vec![FolderNotification {
                folder_id: FolderId("custom".into()),
                folder_name: "Custom".into(),
                count: 1,
            }]
        );
    }

    #[test]
    fn failed_folder_read_preserves_its_last_successful_baseline() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());

        assert!(
            manager
                .observe_folders(
                    &account_id,
                    snapshot(vec![folder(
                        "custom",
                        "Custom",
                        vec![summary("known", 10, 1)],
                    )]),
                )
                .is_empty()
        );
        assert!(
            manager
                .observe_folders(
                    &account_id,
                    NotificationSnapshot {
                        folder_ids: [FolderId("custom".into())].into_iter().collect(),
                        folders: Vec::new(),
                    },
                )
                .is_empty()
        );
        assert_eq!(
            manager.observe_folders(
                &account_id,
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("known", 20, 1)],
                )]),
            ),
            vec![FolderNotification {
                folder_id: FolderId("custom".into()),
                folder_name: "Custom".into(),
                count: 1,
            }]
        );
    }

    #[test]
    fn duplicate_refresh_request_is_coalesced_as_a_follow_up() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        manager.set_fully_active_account(Some(account_id.clone()));
        let refresh = manager
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        let backend = crate::integration::backend::stub_backend();

        assert!(!manager.request_account_refresh(account_id.clone(), MailService::new(backend),));
        assert!(manager.finish_account_refresh(&account_id).is_some());

        drop(refresh);
    }

    #[test]
    fn duplicate_refresh_requests_keep_only_one_follow_up() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        manager.set_fully_active_account(Some(account_id.clone()));
        let refresh = manager
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        let backend = crate::integration::backend::stub_backend();

        for _ in 0..3 {
            assert!(!manager.request_account_refresh(
                account_id.clone(),
                MailService::new(backend.clone()),
            ));
        }
        assert!(manager.finish_account_refresh(&account_id).is_some());
        assert!(manager.finish_account_refresh(&account_id).is_none());

        drop(refresh);
    }

    #[test]
    fn half_active_account_keeps_its_queued_follow_up() {
        let manager = CacheManager::new();
        let first = MailAccountId("account-1".into());
        let second = MailAccountId("account-2".into());
        manager.set_fully_active_account(Some(first.clone()));
        let refresh = manager
            .begin_account_refresh(&first)
            .expect("active account refresh should start");
        let backend = crate::integration::backend::stub_backend();

        assert!(
            !manager.request_account_refresh(first.clone(), MailService::new(backend.clone()),)
        );
        manager.set_fully_active_account(Some(second.clone()));

        assert!(manager.finish_account_refresh(&first).is_some());
        assert!(manager.is_fully_active_account(&second));
        assert!(!manager.is_fully_active_account(&first));
        drop(refresh);
    }

    #[test]
    fn remote_changes_are_coalesced_and_limited_to_the_fully_active_account() {
        let manager = CacheManager::new();
        let publisher = manager.remote_change_publisher();
        let first = MailAccountId("account-1".into());
        let second = MailAccountId("account-2".into());
        manager.set_fully_active_account(Some(first.clone()));

        publisher.publish(&first);
        publisher.publish(&first);
        publisher.publish(&second);
        assert!(matches!(
            manager.drain().as_slice(),
            [CacheEvent::RemoteAccountChanged { account_id }] if account_id == &first
        ));

        manager.acknowledge_remote_change(&first);
        publisher.publish(&first);
        assert!(matches!(
            manager.drain().as_slice(),
            [CacheEvent::RemoteAccountChanged { account_id }] if account_id == &first
        ));

        manager.set_fully_active_account(Some(second.clone()));
        publisher.publish(&first);
        publisher.publish(&second);
        assert!(matches!(
            manager.drain().as_slice(),
            [CacheEvent::RemoteAccountChanged { account_id }] if account_id == &second
        ));
    }

    #[test]
    fn remote_change_callback_does_not_keep_the_manager_alive() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        manager.set_fully_active_account(Some(account_id.clone()));
        let state = Arc::downgrade(&manager.state);
        let publisher = manager.remote_change_publisher();

        drop(manager);

        assert!(state.upgrade().is_none());
        publisher.publish(&account_id);
    }

    #[test]
    fn resetting_a_monitor_invalidates_an_in_flight_connection_attempt() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        manager.set_fully_active_account(Some(account_id.clone()));
        let generation = {
            let mut state = manager
                .state
                .change_monitor
                .lock()
                .expect("change monitor lock should be available");
            state.starting = true;
            state.generation
        };

        manager.reset_account_change_monitor(&account_id);

        let state = manager
            .state
            .change_monitor
            .lock()
            .expect("change monitor lock should be available");
        assert_ne!(state.generation, generation);
        assert!(!state.starting);
        assert!(state.monitor.is_none());
    }

    #[test]
    fn completion_publishes_an_account_refresh_event() {
        let manager = CacheManager::new();
        let refresh = manager
            .begin_account_refresh(&MailAccountId("account-1".into()))
            .expect("refresh should start");

        refresh.complete(None);

        let events = manager.drain();
        assert!(matches!(
            events.as_slice(),
            [CacheEvent::AccountRefreshCompleted {
                account_id,
                failure: None,
            }]
                if account_id.0 == "account-1"
        ));
        assert!(manager.drain().is_empty());
    }

    #[test]
    fn completion_preserves_a_privacy_safe_failure_kind() {
        let manager = CacheManager::new();
        let refresh = manager
            .begin_account_refresh(&MailAccountId("account-1".into()))
            .expect("refresh should start");

        refresh.complete(Some(crate::model::event::RefreshFailureKind::Storage));

        assert!(matches!(
            manager.drain().as_slice(),
            [CacheEvent::AccountRefreshCompleted {
                account_id,
                failure: Some(crate::model::event::RefreshFailureKind::Storage),
            }] if account_id.0 == "account-1"
        ));
    }

    #[test]
    fn cloned_managers_share_events_and_refresh_state() {
        let manager = CacheManager::new();
        let worker = manager.clone();
        let account_id = MailAccountId("account-1".into());
        let refresh = manager
            .begin_account_refresh(&account_id)
            .expect("refresh should start");
        assert!(worker.begin_account_refresh(&account_id).is_none());

        worker.publish(CacheEvent::mailbox_changed(MailAccountId(
            "account-1".into(),
        )));

        let events = manager.drain();
        assert!(matches!(
            events.as_slice(),
            [CacheEvent::MailboxCacheChanged { account_id }]
                if account_id.0 == "account-1"
        ));
        drop(refresh);
    }

    #[test]
    fn dropping_an_unfinished_refresh_releases_it_without_an_event() {
        let manager = CacheManager::new();
        let account_id = MailAccountId("account-1".into());
        let refresh = manager
            .begin_account_refresh(&account_id)
            .expect("refresh should start");

        drop(refresh);

        assert!(manager.drain().is_empty());
        assert!(manager.begin_account_refresh(&account_id).is_some());
    }

    #[test]
    fn drain_preserves_event_order_from_a_single_publisher() {
        let manager = CacheManager::new();
        manager.publish(CacheEvent::mailbox_changed(MailAccountId(
            "account-1".into(),
        )));
        manager.publish(CacheEvent::account_refresh(
            MailAccountId("account-2".into()),
            Some(crate::model::event::RefreshFailureKind::Connectivity),
        ));

        let events = manager.drain();
        assert!(matches!(
            events.as_slice(),
            [
                CacheEvent::MailboxCacheChanged {
                    account_id: first_account_id,
                },
                CacheEvent::AccountRefreshCompleted {
                    account_id: second_account_id,
                    failure: Some(crate::model::event::RefreshFailureKind::Connectivity),
                },
            ] if first_account_id.0 == "account-1" && second_account_id.0 == "account-2"
        ));
    }

    #[test]
    fn external_attachment_without_a_mail_request_is_published_immediately() {
        let manager = CacheManager::new();

        assert!(manager.request_attachment(
            None,
            AttachmentDisposition::Open,
            "report.pdf".into(),
            "file:///tmp/report.pdf".into(),
        ));

        let events = manager.drain();
        assert!(matches!(
            events.as_slice(),
            [CacheEvent::AttachmentPrepared {
                disposition: AttachmentDisposition::Open,
                display_name,
                result: Ok(uri),
            }] if display_name == "report.pdf" && uri == "file:///tmp/report.pdf"
        ));
    }

    #[test]
    fn unresolved_eds_attachment_without_a_mail_request_is_rejected() {
        let manager = CacheManager::new();

        assert!(!manager.request_attachment(
            None,
            AttachmentDisposition::SaveAs,
            "report.pdf".into(),
            "pigeon-eds-attachment:part-1".into(),
        ));
        assert!(manager.drain().is_empty());
    }

    #[test]
    fn attachment_resolution_uses_one_final_uri_or_reports_unavailable() {
        assert_eq!(
            resolve_attachment_uri(
                Some("file:///cache/prepared.pdf".into()),
                "pigeon-eds-attachment:part-1".into(),
            ),
            Ok("file:///cache/prepared.pdf".into())
        );
        assert_eq!(
            resolve_attachment_uri(None, "file:///external/report.pdf".into()),
            Ok("file:///external/report.pdf".into())
        );
        assert_eq!(
            resolve_attachment_uri(None, "pigeon-eds-attachment:part-1".into()),
            Err("Attachment unavailable.".into())
        );
    }

    #[test]
    fn multiple_attachment_results_keep_their_own_user_intent() {
        let manager = CacheManager::new();
        manager.publish(CacheEvent::attachment_prepared(
            AttachmentDisposition::Open,
            "first.pdf".into(),
            Ok("file:///cache/first.pdf".into()),
        ));
        manager.publish(CacheEvent::attachment_prepared(
            AttachmentDisposition::SaveAs,
            "second.png".into(),
            Ok("file:///cache/second.png".into()),
        ));

        let events = manager.drain();
        assert!(matches!(
            events.as_slice(),
            [
                CacheEvent::AttachmentPrepared {
                    disposition: AttachmentDisposition::Open,
                    display_name: first_name,
                    result: Ok(first_uri),
                },
                CacheEvent::AttachmentPrepared {
                    disposition: AttachmentDisposition::SaveAs,
                    display_name: second_name,
                    result: Ok(second_uri),
                },
            ] if first_name == "first.pdf"
                && first_uri == "file:///cache/first.pdf"
                && second_name == "second.png"
                && second_uri == "file:///cache/second.png"
        ));
    }

    #[test]
    fn message_details_are_single_flight_and_keep_only_the_newest_follow_up() {
        let manager = CacheManager::new();
        let backend = crate::integration::backend::stub_backend();
        let account_id = MailAccountId("account-1".into());
        let job = |request_id, conversation_id: &str| MessageDetailJob {
            service: MailService::new(backend.clone()),
            request_id,
            account_id: account_id.clone(),
            conversation_id: ConversationId(conversation_id.into()),
        };

        assert!(manager.begin_message_detail(job(1, "first")));
        assert!(!manager.begin_message_detail(job(2, "second")));
        assert!(!manager.begin_message_detail(job(3, "third")));

        let follow_up = manager
            .finish_message_detail(&account_id)
            .expect("newest detail request should be retained");
        assert_eq!(follow_up.request_id, 3);
        assert_eq!(follow_up.conversation_id, ConversationId("third".into()));
        assert!(manager.finish_message_detail(&account_id).is_none());
        assert!(manager.begin_message_detail(job(4, "fourth")));
    }

    #[test]
    fn message_detail_flights_are_scoped_per_account() {
        let manager = CacheManager::new();
        let backend = crate::integration::backend::stub_backend();
        let job = |account_id: &str, request_id| MessageDetailJob {
            service: MailService::new(backend.clone()),
            request_id,
            account_id: MailAccountId(account_id.into()),
            conversation_id: ConversationId("conversation".into()),
        };

        assert!(manager.begin_message_detail(job("account-1", 1)));
        assert!(manager.begin_message_detail(job("account-2", 2)));
        assert!(!manager.begin_message_detail(job("account-1", 3)));

        let first_follow_up = manager
            .finish_message_detail(&MailAccountId("account-1".into()))
            .expect("first account should retain its follow-up");
        assert_eq!(first_follow_up.request_id, 3);
        assert!(
            manager
                .finish_message_detail(&MailAccountId("account-2".into()))
                .is_none()
        );
    }

    #[test]
    fn clearing_message_selection_discards_its_queued_detail() {
        let manager = CacheManager::new();
        let backend = crate::integration::backend::stub_backend();
        let account_id = MailAccountId("account-1".into());
        let job = |request_id| MessageDetailJob {
            service: MailService::new(backend.clone()),
            request_id,
            account_id: account_id.clone(),
            conversation_id: ConversationId(format!("conversation-{request_id}")),
        };

        assert!(manager.begin_message_detail(job(1)));
        assert!(!manager.begin_message_detail(job(2)));
        manager.cancel_pending_message_detail(&account_id);

        assert!(manager.finish_message_detail(&account_id).is_none());
        assert!(manager.begin_message_detail(job(3)));
    }

    #[test]
    fn searches_are_single_flight_and_keep_only_the_newest_follow_up() {
        let manager = CacheManager::new();
        let backend = crate::integration::backend::stub_backend();
        let job = |request_id, query: &str| SearchJob {
            service: MailService::new(backend.clone()),
            request_id,
            account_id: MailAccountId("account-1".into()),
            query: query.into(),
        };

        assert!(manager.begin_search(job(1, "first")));
        assert!(!manager.begin_search(job(2, "second")));
        assert!(!manager.begin_search(job(3, "third")));

        let follow_up = manager
            .finish_search(&MailAccountId("account-1".into()))
            .expect("newest search should be retained");
        assert_eq!(follow_up.request_id, 3);
        assert_eq!(follow_up.query, "third");
        assert!(
            manager
                .finish_search(&MailAccountId("account-1".into()))
                .is_none()
        );
        assert!(manager.begin_search(job(4, "fourth")));
    }

    #[test]
    fn clearing_a_query_discards_its_queued_follow_up() {
        let manager = CacheManager::new();
        let backend = crate::integration::backend::stub_backend();
        let account_id = MailAccountId("account-1".into());
        let job = |request_id, query: &str| SearchJob {
            service: MailService::new(backend.clone()),
            request_id,
            account_id: account_id.clone(),
            query: query.into(),
        };

        assert!(manager.begin_search(job(1, "first")));
        assert!(!manager.begin_search(job(2, "second")));
        manager.cancel_pending_search(&account_id);
        assert!(manager.finish_search(&account_id).is_none());
        assert!(manager.begin_search(job(3, "third")));
    }
}
