//! The Issues screen's requests, run on a thread of their own so the screen
//! never waits on the network. Of the lists and details asked for while one
//! was running, only the latest of each is read; the others are stale.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use linear::{Change, Comment, Detail, Issue, NewIssue, Team};

use crate::Filters;
use crate::backend::Backend;
use crate::screen::Draft;

pub(crate) enum Request {
    List {
        source: usize,
        seq: u64,
        filters: Filters,
    },
    Detail {
        source: usize,
        id: String,
    },
    Team {
        source: usize,
    },
    /// Change the issue, which looked like `before` until it was changed on
    /// screen.
    Update {
        source: usize,
        before: Box<Issue>,
        change: Box<Change>,
    },
    Comment {
        source: usize,
        issue: Box<Issue>,
        body: String,
    },
    /// Create `new`, put together as `draft`, which `n` starts from again if
    /// it is refused.
    Create {
        source: usize,
        new: Box<NewIssue>,
        draft: Box<Draft>,
    },
}

impl Request {
    /// A request a later one of its kind makes stale.
    fn kind(&self) -> Option<u8> {
        match self {
            Request::List { .. } => Some(0),
            Request::Detail { .. } => Some(1),
            Request::Team { .. }
            | Request::Update { .. }
            | Request::Comment { .. }
            | Request::Create { .. } => None,
        }
    }
}

pub(crate) enum Reply {
    List {
        source: usize,
        seq: u64,
        result: Result<Vec<Issue>, String>,
    },
    Detail {
        source: usize,
        id: String,
        result: Result<Box<Detail>, String>,
    },
    Team {
        source: usize,
        result: Result<Option<Arc<Team>>, String>,
    },
    Updated {
        source: usize,
        before: Box<Issue>,
        result: Result<Box<Issue>, String>,
    },
    Created {
        source: usize,
        draft: Box<Draft>,
        result: Result<Box<Issue>, String>,
    },
    Commented {
        source: usize,
        /// The issue's id, such as `END-123`.
        id: String,
        body: String,
        result: Result<Comment, String>,
    },
}

pub(crate) struct Worker {
    requests: Sender<Request>,
    pub(crate) replies: Receiver<Reply>,
}

fn run(backends: &[Arc<dyn Backend>], request: Request) -> Option<Reply> {
    let reply = match request {
        Request::List {
            source,
            seq,
            filters,
        } => Reply::List {
            source,
            seq,
            result: backends.get(source)?.list(&filters),
        },
        Request::Detail { source, id } => Reply::Detail {
            source,
            result: backends.get(source)?.detail(&id).map(Box::new),
            id,
        },
        Request::Team { source } => Reply::Team {
            source,
            result: backends.get(source)?.team(),
        },
        Request::Update {
            source,
            before,
            change,
        } => Reply::Updated {
            source,
            result: backends.get(source)?.update(&before, &change).map(Box::new),
            before,
        },
        Request::Create { source, new, draft } => Reply::Created {
            source,
            result: backends.get(source)?.create(&new).map(Box::new),
            draft,
        },
        Request::Comment {
            source,
            issue,
            body,
        } => Reply::Commented {
            source,
            result: backends.get(source)?.comment(&issue, &body),
            id: issue.id,
            body,
        },
    };
    Some(reply)
}

impl Worker {
    pub(crate) fn spawn(backends: Vec<Arc<dyn Backend>>) -> Self {
        let (requests, queue) = mpsc::channel::<Request>();
        let (replies_tx, replies) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(first) = queue.recv() {
                let mut batch = vec![first];
                batch.extend(queue.try_iter());
                let latest = |kind: u8| batch.iter().rposition(|r| r.kind() == Some(kind));
                let (list, detail) = (latest(0), latest(1));
                for (at, request) in batch.into_iter().enumerate() {
                    let is_stale = match request.kind() {
                        Some(0) => Some(at) != list,
                        Some(_) => Some(at) != detail,
                        None => false,
                    };
                    if is_stale {
                        continue;
                    }
                    if let Some(reply) = run(&backends, request)
                        && replies_tx.send(reply).is_err()
                    {
                        return;
                    }
                }
            }
        });
        Worker { requests, replies }
    }

    pub(crate) fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}
