//! What the program around the Issues screen knows that the screen does not:
//! which tree and agent are working on an issue.

/// How an issue is being worked on, as the WORK column says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    /// The tree it is picked up in, by its name, such as `4`.
    pub tree: String,
    /// Its agent's state, such as `busy`, `waiting` or `cold`; None for a
    /// tree with no agent.
    pub agent: Option<String>,
    /// The agent is waiting on you.
    pub needs_you: bool,
}

pub trait Host {
    /// Who is working on issue `id`, such as `END-123`, if anyone is.
    fn work_on(&self, id: &str) -> Option<Work>;
}

/// A host with no trees, for a screen on its own.
pub struct NoTrees;

impl Host for NoTrees {
    fn work_on(&self, _id: &str) -> Option<Work> {
        None
    }
}
