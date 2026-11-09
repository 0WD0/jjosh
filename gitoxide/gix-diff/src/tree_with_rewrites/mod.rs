use std::collections::HashSet;

use bstr::BString;

use crate::{Rewrites, tree::recorder::Location};

mod change;
mod directories;
pub use change::{Change, ChangeRef};

/// Returned by the [`tree_with_rewrites()`](super::tree_with_rewrites()) function to control flow.
///
/// Use [`std::ops::ControlFlow::Continue`] to continue the traversal of changes.
/// Use [`std::ops::ControlFlow::Break`] to stop the traversal of changes and stop calling the function that returned it.
pub type Action = std::ops::ControlFlow<()>;

/// Options for use in [`tree_with_rewrites()`](super::tree_with_rewrites()).
#[derive(Default, Clone, Debug)]
pub struct Options {
    /// Determine how locations of changes, i.e. their repository-relative path, should be tracked.
    /// If `None`, locations will always be empty.
    /// Rewrite tracking uses full paths internally regardless of this setting.
    pub location: Option<Location>,
    /// If not `None`, rename tracking will be performed accordingly.
    pub rewrites: Option<Rewrites>,
    /// If not `None`, only additions at these paths are considered as rename or
    /// copy destinations. The set should include parent directories when
    /// directory rename tracking is desired.
    pub rewrite_destination_paths: Option<HashSet<BString>>,
}

pub(super) mod function;
