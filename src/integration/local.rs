//! Account-scoped Drafts, Outbox, and Sent state in EDS local mail.

mod delivery;
mod draft;

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;

use crate::i18n::gettext;
use crate::integration::account::EdsAccountBinding;
use crate::integration::camel::{AccountSession, MessageSyncState};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, FolderKind, MailFolder, MessageDetail,
    sort_and_deduplicate_conversations,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFolder {
    Drafts,
    Outbox,
    Sent,
}

impl LocalFolder {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Drafts => {
                "Drafts"
            }
            Self::Outbox => {
                "Outbox"
            }
            Self::Sent => {
                "Sent"
            }
        }
    }

    pub(crate) fn kind(self) -> FolderKind {
        match self {
            Self::Drafts => {
                FolderKind::Drafts
            }
            Self::Outbox => {
                FolderKind::Outbox
            }
            Self::Sent => {
                FolderKind::Sent
            }
        }
    }

    fn display_name(self) -> String {
        match self {
            Self::Drafts => {
                gettext("Drafts")
            }
            Self::Outbox => {
                gettext("Outbox")
            }
            Self::Sent => {
                gettext("Sent")
            }
        }
    }

    fn from_kind(kind: FolderKind) -> Option<Self> {
        match kind {
            FolderKind::Drafts => {
                Some(Self::Drafts)
            }
            FolderKind::Outbox => {
                Some(Self::Outbox)
            }
            FolderKind::Sent => {
                Some(Self::Sent)
            }
            _ => {
                None
            }
        }
    }
}

pub(crate) struct LocalMailbox<'a> {
    binding: &'a EdsAccountBinding,
}

impl<'a> LocalMailbox<'a> {
    pub(crate) fn new(binding: &'a EdsAccountBinding) -> Self {
        Self { binding }
    }

    pub(super) fn binding(&self) -> &EdsAccountBinding {
        self.binding
    }

    pub(crate) fn folder_id(&self, folder: LocalFolder) -> FolderId {
        FolderId(format!(
            "{}/{}",
            self.binding.account_parent_uid,
            folder.name()
        ))
    }

    pub(crate) fn folder_uri(&self, folder: LocalFolder) -> anyhow::Result<String> {
        crate::integration::camel::folder_uri("local", &self.folder_id(folder))
    }

    pub(crate) fn contains(&self, conversation_id: &ConversationId) -> bool {
        [LocalFolder::Drafts, LocalFolder::Outbox, LocalFolder::Sent]
            .into_iter()
            .any(|folder| self.contains_in(folder, conversation_id))
    }

    fn contains_in(
        &self,
        folder: LocalFolder,
        conversation_id: &ConversationId,
    ) -> bool {
        let folder_id = self.folder_id(folder);
        conversation_folder_id(conversation_id)
            .is_some_and(|candidate| candidate == folder_id.0)
    }

    pub(crate) fn folder_for_projection(
        &self,
        folders: &[MailFolder],
        folder_id: &FolderId,
    ) -> Option<LocalFolder> {
        folders
            .iter()
            .find(|folder| folder.id == *folder_id)
            .and_then(|folder| LocalFolder::from_kind(folder.kind))
    }

    fn open(&self) -> anyhow::Result<AccountSession> {
        AccountSession::open_local_mailbox()
    }

    pub(crate) fn ensure_folders(&self) -> anyhow::Result<()> {
        let mut session = self.open()?;
        for folder in [
            LocalFolder::Drafts,
            LocalFolder::Outbox,
            LocalFolder::Sent,
        ] {
            session.ensure_folder_path(&self.folder_id(folder))?;
        }
        Ok(())
    }

    /// Loads the user-visible local side of a mailbox role without touching
    /// the network. Delivery acknowledgement, rather than the physical
    /// Maildir folder alone, determines whether a record belongs in Outbox or
    /// Sent after an interrupted move.
    pub(crate) fn load_projection(
        &self,
        folder: LocalFolder,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        match folder {
            LocalFolder::Drafts => {
                self.load_drafts()
            }
            LocalFolder::Outbox => {
                Ok(self.load_outbox_projection()?.outbox)
            }
            LocalFolder::Sent => {
                self.load_sent_projection()
            }
        }
    }

    pub(crate) fn ensure_projection_folders(
        &self,
        folders: &mut Vec<MailFolder>,
    ) -> anyhow::Result<()> {
        for folder in [LocalFolder::Drafts, LocalFolder::Sent] {
            self.ensure_projection_folder(folders, folder);
        }
        if self.has_outbox_messages()? {
            self.ensure_projection_folder(folders, LocalFolder::Outbox);
        }
        Ok(())
    }

    pub(crate) fn ensure_projection_folder(
        &self,
        folders: &mut Vec<MailFolder>,
        folder: LocalFolder,
    ) -> FolderId {
        let display_folder_id = self.projected_folder_id(folders, folder);
        ensure_projected_folder(folders, &display_folder_id, folder);
        display_folder_id
    }

    pub(crate) fn merge_cached_role(
        &self,
        folder: LocalFolder,
        display_folder_id: &FolderId,
        current: &mut Vec<ConversationSummary>,
    ) -> anyhow::Result<()> {
        replace_projected_rows(
            current,
            self.load_projection(folder)?,
            &self.folder_id(folder),
            display_folder_id,
        );
        Ok(())
    }

    pub(crate) fn project_folder(
        &self,
        folder: LocalFolder,
        local_conversations: Vec<ConversationSummary>,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
    ) {
        let display_folder_id = self.ensure_projection_folder(folders, folder);
        replace_projected_rows(
            conversations.entry(display_folder_id.clone()).or_default(),
            local_conversations,
            &self.folder_id(folder),
            &display_folder_id,
        );
        update_unread_count(folders, &display_folder_id, conversations);
    }

    pub(crate) fn project_delivery(
        &self,
        outbox: Vec<ConversationSummary>,
        sent: Vec<ConversationSummary>,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
    ) {
        if !outbox.is_empty() {
            self.project_folder(
                LocalFolder::Outbox,
                outbox,
                folders,
                conversations,
            );
        }
        self.project_folder(
            LocalFolder::Sent,
            sent,
            folders,
            conversations,
        );
    }

    pub(crate) fn provider_folder<'b>(
        &self,
        folders: &'b [MailFolder],
        folder: LocalFolder,
    ) -> Option<&'b MailFolder> {
        folders
            .iter()
            .find(|candidate| candidate.kind == folder.kind())
    }

    fn projected_folder_id(
        &self,
        folders: &[MailFolder],
        folder: LocalFolder,
    ) -> FolderId {
        self.provider_folder(folders, folder)
            .map(|candidate| candidate.id.clone())
            .unwrap_or_else(|| self.folder_id(folder))
    }

    pub(crate) fn message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.get_message_detail(conversation_id)
    }

    pub(crate) fn export_attachment(
        &self,
        conversation_id: &ConversationId,
        attachment_token: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.export_attachment(conversation_id, attachment_token)
    }

    pub(crate) fn set_read(
        &self,
        conversation_id: &ConversationId,
        read: bool,
    ) -> anyhow::Result<()> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.set_read(conversation_id, read)
    }

    pub(crate) fn set_starred(
        &self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> anyhow::Result<()> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.set_starred(conversation_id, starred)
    }

    pub(crate) fn search(
        &self,
        query: &str,
        visible_folders: &[MailFolder],
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut session = self.open()?;
        let mut results = self.search_drafts(&mut session, query, visible_folders)?;
        results.extend(self.search_delivery(&mut session, query, visible_folders)?);
        Ok(results)
    }
}

fn message_states_by_id(
    states: Vec<MessageSyncState>,
) -> anyhow::Result<HashMap<ConversationId, MessageSyncState>> {
    let mut states_by_id = HashMap::with_capacity(states.len());
    for state in states {
        let conversation_id = state.conversation_id.clone();
        if states_by_id.insert(conversation_id.clone(), state).is_some() {
            return Err(anyhow!(
                "local message '{}' has duplicate Camel synchronization state",
                conversation_id.0
            ));
        }
    }
    Ok(states_by_id)
}

fn ensure_projected_folder(
    folders: &mut Vec<MailFolder>,
    folder_id: &FolderId,
    folder: LocalFolder,
) {
    if folders.iter().any(|candidate| candidate.id == *folder_id) {
        return;
    }
    folders.push(MailFolder {
        id: folder_id.clone(),
        name: folder.display_name(),
        unread_count: 0,
        kind: folder.kind(),
    });
}

fn replace_projected_rows(
    current: &mut Vec<ConversationSummary>,
    mut projected: Vec<ConversationSummary>,
    local_folder_id: &FolderId,
    display_folder_id: &FolderId,
) {
    let projected_ids = projected
        .iter()
        .map(|summary| &summary.id)
        .collect::<HashSet<_>>();
    current.retain(|summary| {
        !projected_ids.contains(&summary.id)
            && conversation_folder_id(&summary.id) != Some(local_folder_id.0.as_str())
    });
    for summary in &mut projected {
        summary.folder_id = display_folder_id.clone();
    }
    current.extend(projected);
    sort_and_deduplicate_conversations(current);
}

fn update_unread_count(
    folders: &mut [MailFolder],
    folder_id: &FolderId,
    conversations: &HashMap<FolderId, Vec<ConversationSummary>>,
) {
    let unread_count = conversations
        .get(folder_id)
        .into_iter()
        .flatten()
        .map(|conversation| conversation.unread_count)
        .sum();
    if let Some(folder) = folders.iter_mut().find(|folder| folder.id == *folder_id) {
        folder.unread_count = unread_count;
    }
}

fn conversation_folder_id(conversation_id: &ConversationId) -> Option<&str> {
    crate::integration::camel::conversation_id_parts(&conversation_id.0)
        .map(|(folder_name, _)| folder_name)
}

fn refresh_conversation_folder(
    session: &mut AccountSession,
    conversation_id: &ConversationId,
) -> anyhow::Result<()> {
    let folder_name = conversation_folder_id(conversation_id)
        .ok_or_else(|| anyhow!("local conversation id has no folder component"))?;
    session.refresh_folder_info(&FolderId(folder_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::account::RouteFingerprint;
    use crate::model::account::MailAccountId;

    fn binding() -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: MailAccountId("account-1".into()),
            account_uid: "account-source".into(),
            account_parent_uid: "collection-source".into(),
            account_backend_name: "imapx".into(),
            account_auth_method: None,
            identity_uid: "identity-source".into(),
            transport_uid: "transport-source".into(),
            transport_backend_name: "smtp".into(),
            transport_auth_method: None,
            route_fingerprint: RouteFingerprint {
                account: 0,
                transport: 0,
                mailbox: 0,
            },
        }
    }

    #[test]
    fn folder_ids_and_conversation_ownership_share_one_collection_scope() {
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        assert_eq!(
            mailbox.folder_id(LocalFolder::Outbox),
            FolderId("collection-source/Outbox".into())
        );
        assert_eq!(
            mailbox.folder_uri(LocalFolder::Drafts).unwrap(),
            "folder://local/collection-source/Drafts"
        );
        assert_eq!(
            mailbox.folder_uri(LocalFolder::Sent).unwrap(),
            "folder://local/collection-source/Sent"
        );
        assert!(mailbox.contains(&ConversationId(
            "collection-source/Outbox\u{1f}uid-1".into()
        )));
        assert!(mailbox.contains(&ConversationId(
            "collection-source/Sent\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source-other/Outbox\u{1f}uid-1".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source/Other\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source/Drafts/Nested\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId("malformed".into())));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source\u{1f}uid-3".into()
        )));
    }

    #[test]
    fn draft_replacement_cannot_cross_local_collections_or_folder_roles() {
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        assert!(mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Drafts\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Outbox\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("other/Drafts\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Sent\u{1f}old".into()),
        ));
    }

    #[test]
    fn local_projection_replaces_only_its_previous_local_rows() {
        let summary = |id: &str| ConversationSummary {
            id: ConversationId(id.into()),
            folder_id: FolderId(
                id.split_once('\u{1f}')
                    .expect("fixture conversation IDs contain a folder")
                    .0
                    .into(),
            ),
            subject: id.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 1,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: String::new(),
        };
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        let mut folders = vec![MailFolder {
            id: FolderId("remote-drafts".into()),
            name: "Drafts".into(),
            unread_count: 0,
            kind: FolderKind::Drafts,
        }];
        let mut conversations = HashMap::from([(
            FolderId("remote-drafts".into()),
            vec![
                summary("remote-drafts\u{1f}remote"),
                summary("collection-source/Drafts\u{1f}old-local"),
            ],
        )]);

        mailbox.project_folder(
            LocalFolder::Drafts,
            vec![summary("collection-source/Drafts\u{1f}new-local")],
            &mut folders,
            &mut conversations,
        );

        let ids = conversations[&FolderId("remote-drafts".into())]
            .iter()
            .map(|summary| summary.id.0.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            ids,
            HashSet::from([
                "remote-drafts\u{1f}remote",
                "collection-source/Drafts\u{1f}new-local",
            ])
        );
        assert!(
            conversations[&FolderId("remote-drafts".into())]
                .iter()
                .all(|summary| summary.folder_id == FolderId("remote-drafts".into()))
        );
        assert_eq!(folders[0].unread_count, 2);
    }
}
