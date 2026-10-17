//! The Issues screen's requests, run on a thread of their own so the screen
//! never waits on the network. Of the lists and details asked for while one
//! was running, only the latest of each is read; the others are stale.
//! Images are fetched and decoded on a second thread, so a large one never
//! holds a list up.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use image::DynamicImage;
use linear::{Change, Comment, Detail, Issue, NewIssue, Team};

use crate::Filters;
use crate::backend::Backend;
use crate::form::Form;

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
    /// screen, in `form` when it was written in one.
    Update {
        source: usize,
        before: Box<Issue>,
        change: Box<Change>,
        form: Option<Box<Form>>,
    },
    Comment {
        source: usize,
        issue: Box<Issue>,
        body: String,
    },
    /// Fetch and decode the image at `url`.
    Image {
        source: usize,
        url: String,
    },
    /// Create `new`, written in `form`, which opens again if it is refused.
    Create {
        source: usize,
        new: Box<NewIssue>,
        form: Box<Form>,
    },
}

impl Request {
    /// A request a later one of its kind makes stale.
    fn kind(&self) -> Option<u8> {
        match self {
            Request::List { .. } => Some(0),
            Request::Detail { .. } => Some(1),
            Request::Team { .. }
            | Request::Image { .. }
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
        form: Option<Box<Form>>,
        result: Result<Box<Issue>, String>,
    },
    Created {
        source: usize,
        form: Box<Form>,
        result: Result<Box<Issue>, String>,
    },
    Commented {
        source: usize,
        /// The issue's id, such as `END-123`.
        id: String,
        body: String,
        result: Result<Comment, String>,
    },
    Image {
        url: String,
        result: Result<Box<DynamicImage>, String>,
    },
}

/// The longest side an image is kept at: more than any terminal shows, and
/// a pasted 4K screenshot would otherwise sit in memory whole.
const IMAGE_SIDE: u32 = 2400;

/// The image `bytes` hold, made no bigger than IMAGE_SIDE.
fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|err| format!("not an image treetop reads: {err}"))?;
    if image.width().max(image.height()) > IMAGE_SIDE {
        return Ok(image.resize(
            IMAGE_SIDE,
            IMAGE_SIDE,
            image::imageops::FilterType::Triangle,
        ));
    }
    Ok(image)
}

pub(crate) struct Worker {
    requests: Sender<Request>,
    images: Sender<Request>,
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
            form,
        } => Reply::Updated {
            source,
            result: backends.get(source)?.update(&before, &change).map(Box::new),
            before,
            form,
        },
        Request::Create { source, new, form } => Reply::Created {
            source,
            result: backends.get(source)?.create(&new).map(Box::new),
            form,
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
        Request::Image { source, url } => Reply::Image {
            result: backends
                .get(source)?
                .fetch(&url)
                .and_then(|bytes| decode(&bytes))
                .map(Box::new),
            url,
        },
    };
    Some(reply)
}

impl Worker {
    pub(crate) fn spawn(backends: Vec<Arc<dyn Backend>>) -> Self {
        let (requests, queue) = mpsc::channel::<Request>();
        let (replies_tx, replies) = mpsc::channel();
        let (images, image_queue) = mpsc::channel::<Request>();
        let image_backends = backends.clone();
        let image_replies = replies_tx.clone();
        thread::spawn(move || {
            while let Ok(request) = image_queue.recv() {
                if let Some(reply) = run(&image_backends, request)
                    && image_replies.send(reply).is_err()
                {
                    return;
                }
            }
        });
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
        Worker {
            requests,
            images,
            replies,
        }
    }

    pub(crate) fn send(&self, request: Request) {
        let queue = match request {
            Request::Image { .. } => &self.images,
            _ => &self.requests,
        };
        let _ = queue.send(request);
    }
}
