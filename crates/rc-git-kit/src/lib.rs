//! App state and pure text logic for the rc-git UI.
//!
//! Everything here is decided once and tested without a window: the views in `rc-git-app`
//! only render what this crate hands them. Each module names the Swift file it replaces.

pub mod markdown;
pub mod import_resolver;
pub mod repo_path;
pub mod path_search;
pub mod text_document;
pub mod commit_graph;
pub mod ref_filter;
pub mod natural_sort;
pub mod change_tree;
pub mod layout_policy;
pub mod branch_banner;
pub mod menu_shortcuts;
pub mod back_forward;
pub mod commit_identity;
pub mod update_copy;
pub mod app_build;
pub mod restore_notice;
pub mod tree_placeholder;
pub mod shell_word;
pub mod canonical_path;
pub mod hidden_files;
pub mod recent_repositories;
pub mod tabs;
pub mod guard_bridge;
pub mod diff_text;
