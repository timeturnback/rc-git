//! Port of `CommitAuthorModel` (`CommitIdentity.swift`): keeps the author line beside the
//! Commit button current for one repository. The repository's config is read off the UI
//! thread through [`RepositoryActions::configured_identity`], and each read carries a
//! generation, so a slow read never overwrites a newer answer.

use std::sync::Arc;

use gpui::{Context, EventEmitter, Task};

use crate::blocking::blocking;
use crate::commit_identity::{CommitAuthor, CommitIdentity};
use crate::platform::settings::SettingsError;
use crate::platform::stores::CommitIdentityStore;
use crate::repository_actions::RepositoryActions;

/// [`CommitAuthorModel::author`] was re-decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorChanged;

/// Who the next commit or stash of one repository will be recorded as.
pub struct CommitAuthorModel {
    repo_path: String,
    author: Option<CommitAuthor>,
    store: CommitIdentityStore,
    actions: Arc<dyn RepositoryActions>,
    generation: u64,
}

impl EventEmitter<AuthorChanged> for CommitAuthorModel {}

impl CommitAuthorModel {
    /// The author model of the repository at `repo_path`, which `actions` act on; saved
    /// identities live in `store`.
    #[must_use]
    pub fn new(
        repo_path: impl Into<String>,
        store: CommitIdentityStore,
        actions: Arc<dyn RepositoryActions>,
    ) -> Self {
        Self {
            repo_path: repo_path.into(),
            author: None,
            store,
            actions,
            generation: 0,
        }
    }

    /// The repository this model decides for.
    #[must_use]
    pub fn repo_path(&self) -> &str {
        &self.repo_path
    }

    /// The decided author; `None` until the first read lands.
    #[must_use]
    pub fn author(&self) -> Option<&CommitAuthor> {
        self.author.as_ref()
    }

    /// The fallback to hand the guard: the identity saved for this repository.
    #[must_use]
    pub fn saved(&self) -> Option<CommitIdentity> {
        self.store.identity(&self.repo_path)
    }

    /// Re-reads the repository's config and re-decides the author.
    pub fn refresh(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.generation += 1;
        let generation = self.generation;
        let actions = Arc::clone(&self.actions);
        let read = blocking(cx.background_executor(), move || {
            actions.configured_identity()
        });
        cx.spawn(async move |this, cx| {
            let configured = read.await;
            let _ = this.update(cx, |model, cx| {
                if generation != model.generation {
                    return;
                }
                model.author = Some(match configured {
                    Ok(configured) => {
                        let configured = configured.map(|identity| CommitIdentity {
                            name: identity.name,
                            email: identity.email,
                        });
                        CommitAuthor::resolve(configured, model.saved())
                    }
                    Err(error) => CommitAuthor::Unreadable(error.to_string()),
                });
                cx.emit(AuthorChanged);
                cx.notify();
            });
        })
    }

    /// Saves `identity` for this repository, then re-decides the author.
    ///
    /// # Errors
    /// [`SettingsError`] when the identity could not be saved; nothing is re-read then.
    pub fn save(
        &mut self,
        identity: &CommitIdentity,
        cx: &mut Context<Self>,
    ) -> Result<Task<()>, SettingsError> {
        self.store.save(identity, &self.repo_path)?;
        Ok(self.refresh(cx))
    }

    /// Forgets the identity saved for this repository, then re-decides the author.
    ///
    /// # Errors
    /// [`SettingsError`] when the store could not be written; nothing is re-read then.
    pub fn clear(&mut self, cx: &mut Context<Self>) -> Result<Task<()>, SettingsError> {
        self.store.clear(&self.repo_path)?;
        Ok(self.refresh(cx))
    }
}
