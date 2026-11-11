//! Port of `CommitIdentity.swift`: parsing and validating the identity a commit or stash is
//! recorded as, the per-repository fallback rc-git remembers, and the author line beside the
//! Commit button. Storage belongs to the settings store; [`SavedIdentities`] is the shape it
//! keeps.

use std::collections::BTreeMap;
use std::fmt;

/// Who a commit or stash is recorded as: `user.name` and `user.email`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitIdentity {
    /// `user.name`.
    pub name: String,
    /// `user.email`.
    pub email: String,
}

/// Which field of an identity a problem is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityField {
    /// `user.name`.
    Name,
    /// `user.email`.
    Email,
}

/// Why typed input is not an identity git can record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityProblem {
    /// Nothing is left of the field after trimming.
    Empty(IdentityField),
    /// The field holds `<`, `>`, or a control character (line breaks included).
    ForbiddenCharacter(IdentityField),
}

impl IdentityProblem {
    /// One sentence the identity sheet shows under its fields.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty(IdentityField::Name) => "Enter a name.",
            Self::Empty(IdentityField::Email) => "Enter an email address.",
            Self::ForbiddenCharacter(IdentityField::Name) => {
                "The name cannot contain “<”, “>”, or line breaks. Enter the email in its own field."
            }
            Self::ForbiddenCharacter(IdentityField::Email) => {
                "Enter the email without “<” or “>”, and on one line."
            }
        }
    }
}

impl fmt::Display for IdentityProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for IdentityProblem {}

impl CommitIdentity {
    /// Turns typed input into an identity git can record. Both fields are trimmed of
    /// surrounding Unicode whitespace and newlines; what is left must be non-empty and free
    /// of `<`, `>` and control characters (newlines included). git's signature format puts
    /// the email between angle brackets on one line, so the guard refuses those characters at
    /// write time; checking here keeps a bad identity from ever being saved.
    ///
    /// # Errors
    /// [`IdentityProblem::Empty`] for the first field left empty after trimming, name first;
    /// then [`IdentityProblem::ForbiddenCharacter`] for the first field holding a refused
    /// character.
    pub fn parse(name: &str, email: &str) -> Result<Self, IdentityProblem> {
        let name = name.trim();
        let email = email.trim();
        if name.is_empty() {
            return Err(IdentityProblem::Empty(IdentityField::Name));
        }
        if email.is_empty() {
            return Err(IdentityProblem::Empty(IdentityField::Email));
        }
        if !is_recordable(name) {
            return Err(IdentityProblem::ForbiddenCharacter(IdentityField::Name));
        }
        if !is_recordable(email) {
            return Err(IdentityProblem::ForbiddenCharacter(IdentityField::Email));
        }
        Ok(Self {
            name: name.to_owned(),
            email: email.to_owned(),
        })
    }

    /// `Name <email>`, the way git prints an author.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }
}

fn is_recordable(field: &str) -> bool {
    for scalar in field.chars() {
        if scalar == '<' || scalar == '>' || scalar.is_control() {
            return false;
        }
    }
    true
}

/// One stored entry as the settings store keeps it: either field may be missing when another
/// build wrote the key with a different shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedIdentity {
    /// The stored `name`, if any.
    pub name: Option<String>,
    /// The stored `email`, if any.
    pub email: Option<String>,
}

/// The identity rc-git remembers for each repository, keyed by repository path. It is only
/// ever a fallback: the guard records it when that repository's own config sets no
/// `user.name`/`user.email`, and one repository's answer never applies to another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedIdentities {
    /// Normalized repository path to its stored entry.
    pub entries: BTreeMap<String, SavedIdentity>,
}

impl SavedIdentities {
    /// The key the settings store keeps this map under.
    pub const KEY: &'static str = "rc-git.commitIdentities";

    /// The identity saved for `repo_path`. A stored value that no longer parses is treated as
    /// absent, so it can never break a commit.
    #[must_use]
    pub fn identity(&self, repo_path: &str) -> Option<CommitIdentity> {
        let entry = self.entries.get(&standardized_path(repo_path))?;
        let (Some(name), Some(email)) = (&entry.name, &entry.email) else {
            return None;
        };
        CommitIdentity::parse(name, email).ok()
    }

    /// Remembers `identity` for `repo_path`.
    pub fn save(&mut self, identity: &CommitIdentity, repo_path: &str) {
        self.entries.insert(
            standardized_path(repo_path),
            SavedIdentity {
                name: Some(identity.name.clone()),
                email: Some(identity.email.clone()),
            },
        );
    }

    /// Forgets the identity saved for `repo_path`.
    pub fn clear(&mut self, repo_path: &str) {
        self.entries.remove(&standardized_path(repo_path));
    }
}

/// One key per folder however the path was spelled: empty and `.` components dropped, no
/// trailing slash, and in an absolute path each `..` removes the component before it.
///
/// Lexical only, unlike `NSString.standardizingPath`: no tilde expansion, no symlink
/// resolution, and `/private` is kept, matching the spelling `CanonicalPath` saves.
#[must_use]
pub fn standardized_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if absolute => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Who the next commit or stash will be recorded as, decided in the guard's order: the
/// repository's own config, else the identity saved for it, else nobody yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommitAuthor {
    /// The repository's own config names the author.
    Configured(CommitIdentity),
    /// The identity rc-git saved for this repository.
    Saved(CommitIdentity),
    /// Nobody yet; Commit asks for one.
    Unset,
    /// The repository's config could not be read; the message says why.
    Unreadable(String),
}

impl CommitAuthor {
    /// The configured identity when there is one, else the saved one, else [`Self::Unset`].
    #[must_use]
    pub fn resolve(configured: Option<CommitIdentity>, saved: Option<CommitIdentity>) -> Self {
        if let Some(configured) = configured {
            return Self::Configured(configured);
        }
        if let Some(saved) = saved {
            return Self::Saved(saved);
        }
        Self::Unset
    }

    /// The line shown next to the Commit button.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Configured(identity) => {
                format!("Author: {} (repository config)", identity.display())
            }
            Self::Saved(identity) => format!("Author: {} (saved in rc-git)", identity.display()),
            Self::Unset => "Author: none set; Commit asks for one".to_owned(),
            Self::Unreadable(_) => "Author: could not read this repository’s config".to_owned(),
        }
    }
}
