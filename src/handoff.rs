//! Handing an agent's work on to a fresh one in the same tree, for when its
//! context has filled with more than the work needs. The old agent knows
//! best what is done, what is left and what it tried, so it is asked to
//! write the new one's prompt to a file in the tree. treetop reads it, opens
//! it in the compose box to be read over, and starts the new agent on it.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long an agent gets to write its handoff before treetop stops
/// waiting for it.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// Where the agent in `tree` writes its handoff, named for `now`, seconds
/// since the epoch, so one left from an earlier handoff is never taken for
/// it. It sits in the tree, where the agent may write without asking;
/// treetop removes it once read.
pub fn file(tree: &Path, now: u64) -> PathBuf {
    tree.join(format!(".treetop-handoff-{now}.md"))
}

/// What the agent is asked, to write its handoff to `file`.
pub fn request(file: &Path) -> String {
    format!(
        "Your context is filling up, so this work is moving to a fresh agent that will carry on \
in this same tree and branch, with none of this conversation. Write the prompt you would want \
to be handed, to {}, and stop there: do no more of the work, and do not commit the file, which \
treetop takes out of the tree once it has read it, so the new agent never finds it there. Make \
it stand on its own, since the new agent reads nothing else: the task and what done looks \
like; what is done, and whether it is committed; what is left, in order; the decisions made \
and why; what was tried and failed; the files that matter; and anything the user asked for \
that must still hold.",
        file.display()
    )
}
