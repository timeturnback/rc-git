//! App state and pure text logic for the rc-git UI.
//!
//! Everything here is decided once and tested without a window: the views in `rc-git-app`
//! only render what this crate hands them. Each module names the Swift file it replaces.

pub mod app_build;
pub mod back_forward;
pub mod blocking;
pub mod branch_banner;
pub mod canonical_path;
pub mod change_tree;
pub mod commit_graph;
pub mod commit_identity;
pub mod diff_text;
#[cfg(feature = "test-support")]
pub mod fake;
pub mod guard_bridge;
pub mod hidden_files;
pub mod history_paging;
pub mod import_resolver;
pub mod layout_policy;
pub mod markdown;
pub mod menu_shortcuts;
pub mod models;
pub mod natural_sort;
pub mod oneshot;
pub mod path_search;
pub mod platform;
pub mod recent_repositories;
pub mod ref_filter;
pub mod repo_path;
pub mod repository;
pub mod repository_actions;
pub mod restore_notice;
pub mod shell_word;
pub mod tabs;
pub mod text_document;
pub mod tree_placeholder;
pub mod tree_store;
pub mod update_copy;
