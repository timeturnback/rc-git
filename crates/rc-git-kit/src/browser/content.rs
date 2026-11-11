//! Find in Repository: a literal search of every file's contents below the root, over the
//! file list path search builds, and opening a hit at its line.

use gpui::{Context, Task};

use super::{BrowserModel, with};
use crate::content_search::{
    ContentLineMatch, ContentReport, ContentSearchProgress, ContentSearchRequest, search_blobs,
    search_worktree, searchable,
};
use crate::path_search::PathIndexEntry;
use crate::path_search_session::PathSearchError;
use crate::repository::RepositoryError;
use crate::text_document::FileReveal;

/// Why a content search brought nothing back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentSearchError {
    /// A newer search superseded it; nothing to show.
    Cancelled,
    /// The file list or a read failed.
    Failed(RepositoryError),
}

impl From<PathSearchError> for ContentSearchError {
    fn from(error: PathSearchError) -> Self {
        match error {
            PathSearchError::Cancelled => Self::Cancelled,
            PathSearchError::Failed(message) => Self::Failed(RepositoryError::new(message)),
        }
    }
}

impl BrowserModel {
    /// Searches the contents of every file below the current root for the literal `query`,
    /// with smart case: on disk while browsing the working tree, else the blobs, each blob
    /// read once however many paths share it. It reads the file list path search uses,
    /// building it on first use. Hidden files are searched only while they are shown.
    ///
    /// Hits reach `report` a batch at a time, in file order, while this search is the newest;
    /// a newer call makes this one stop reporting and end with
    /// [`ContentSearchError::Cancelled`].
    ///
    /// # Errors
    /// [`ContentSearchError::Cancelled`] when superseded; [`ContentSearchError::Failed`] when
    /// no repository is open, the index build failed, or a read failed.
    pub fn search_content(
        &mut self,
        query: &str,
        report: ContentReport,
        cx: &mut Context<Self>,
    ) -> Task<Result<ContentSearchProgress, ContentSearchError>> {
        self.generations.content_search += 1;
        let generation = self.generations.content_search;
        let (Some(repository), Some(root)) = (self.repository.clone(), self.root_tree) else {
            return Task::ready(Err(ContentSearchError::Failed(RepositoryError::new(
                "content search: no repository is open",
            ))));
        };
        let worktree = self.browsing_working_tree;
        let include_hidden = self.shows_hidden_files;
        let index = self.current_path_index(root, cx);
        let query = query.to_owned();
        Self::run(
            cx,
            Err(ContentSearchError::Cancelled),
            async move |this, cx| {
                let index = index.await?;
                let current = with(&this, cx, |model, _| {
                    generation == model.generations.content_search
                });
                if current != Some(true) {
                    return Err(ContentSearchError::Cancelled);
                }
                let request = ContentSearchRequest {
                    query,
                    files: searchable(&index.entries, include_hidden),
                    index_truncated: index.truncated,
                };
                let app = cx.clone();
                let model = this.clone();
                let forward = move |hits, progress: &ContentSearchProgress| {
                    let current = model
                        .read_with(&app, |model, _| {
                            generation == model.generations.content_search
                        })
                        .unwrap_or(false);
                    if current {
                        let progress = *progress;
                        app.update(|cx| report(hits, progress, cx));
                    }
                };
                let executor = cx.background_executor().clone();
                let searched = if worktree {
                    search_worktree(&executor, repository, request, forward).await
                } else {
                    search_blobs(&executor, repository, request, forward).await
                };
                let progress = searched.map_err(ContentSearchError::Failed)?;
                let current = with(&this, cx, |model, _| {
                    generation == model.generations.content_search
                });
                if current != Some(true) {
                    return Err(ContentSearchError::Cancelled);
                }
                Ok(progress)
            },
        )
    }

    /// Opens a content-search hit as path search opens a pick, and has the pane select
    /// `found` once it paints; `None` opens the file at the top.
    pub fn open_content_match(
        &mut self,
        entry: &PathIndexEntry,
        found: Option<&ContentLineMatch>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let reveal = found.map(|found| FileReveal::Match {
            line: found.line,
            byte_column: found.column,
            byte_length: found.match_length,
        });
        self.open_pick(entry, reveal, cx)
    }
}
