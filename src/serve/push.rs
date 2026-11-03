//! Push to the phone: Web Push, which iOS delivers to a web app added to
//! the home screen. The server keeps a VAPID key pair, which tells the
//! push service the messages are its own, and one subscription per device
//! that allowed notifications. A message is encrypted for the device as RFC
//! 8291 says (`aes128gcm`), so the push service carries it unread, and it
//! names a tree and what happened to it, never what its agent wrote.
//!
//! The crypto is `ring`'s, which treetop already carries for TLS.
//!
//! Files, under treetop's config directory:
//! - `push/vapid.pk8`: the VAPID key pair, made the first time it is needed.
//! - `push/subscriptions.json`: every device's subscription, as the browser
//!   gave it. One the push service says is gone is dropped.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
use ring::{aead, agreement, hkdf};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::Status;
use crate::app::App;
use crate::reports::State;
use crate::ship::Stage;

use super::pool::title;

/// How long a push service keeps a message for a phone that is off.
const TTL: &str = "86400";

/// How long a send to the push service may take.
const SEND_LIMIT: Duration = Duration::from_secs(15);

/// A browser's subscription: where to send, and the keys to encrypt for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionKeys {
    /// The browser's P-256 public key, uncompressed, base64url.
    pub p256dh: String,
    /// Its 16-byte authentication secret, base64url.
    pub auth: String,
}

/// The server's VAPID key pair and the devices that asked for pushes.
pub struct Push {
    dir: PathBuf,
    vapid: EcdsaKeyPair,
    /// Who to say the messages come from, as VAPID's `sub` claim.
    contact: String,
    subscriptions: Mutex<Vec<Subscription>>,
    rng: SystemRandom,
}

impl Push {
    /// The push state kept in `dir`, its key pair made if there is none.
    pub fn open(dir: PathBuf, owner: &str) -> Result<Push, String> {
        let rng = SystemRandom::new();
        fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let key_file = dir.join("vapid.pk8");
        let pkcs8 = match fs::read(&key_file) {
            Ok(bytes) => bytes,
            Err(_) => {
                let made = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                    .map_err(|_| "could not make a VAPID key".to_string())?;
                write_private(&key_file, made.as_ref())?;
                made.as_ref().to_vec()
            }
        };
        let vapid = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
            .map_err(|_| format!("{}: not a VAPID key", key_file.display()))?;
        let subscriptions = fs::read_to_string(dir.join("subscriptions.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let contact = if owner.contains('@') {
            format!("mailto:{owner}")
        } else {
            "https://github.com/anthonyb8/treetop".into()
        };
        Ok(Push {
            dir,
            vapid,
            contact,
            subscriptions: Mutex::new(subscriptions),
            rng,
        })
    }

    /// The public key a browser subscribes with, base64url.
    pub fn public_key(&self) -> String {
        B64.encode(self.vapid.public_key().as_ref())
    }

    /// Keeps `subscription`, in place of any with the same endpoint.
    pub fn subscribe(&self, subscription: Subscription) -> Result<(), String> {
        let mut all = self.lock();
        all.retain(|s| s.endpoint != subscription.endpoint);
        all.push(subscription);
        self.save(&all)
    }

    /// Forgets the device whose subscription goes to `endpoint`.
    pub fn unsubscribe(&self, endpoint: &str) -> Result<(), String> {
        let mut all = self.lock();
        all.retain(|s| s.endpoint != endpoint);
        self.save(&all)
    }

    /// Sends `notice` to every device, dropping those the push service
    /// says are gone.
    pub fn send(&self, notice: &Notice) {
        let payload = serde_json::to_vec(&notice.json()).unwrap_or_default();
        let devices = self.lock().clone();
        let mut gone = Vec::new();
        for device in &devices {
            match self.deliver(device, &payload) {
                Ok(()) => {}
                Err(Sent::Gone) => gone.push(device.endpoint.clone()),
                Err(Sent::Failed(why)) => eprintln!("treetop serve: push: {why}"),
            }
        }
        if !gone.is_empty() {
            let mut all = self.lock();
            all.retain(|s| !gone.contains(&s.endpoint));
            let _ = self.save(&all);
        }
    }

    fn deliver(&self, device: &Subscription, payload: &[u8]) -> Result<(), Sent> {
        let body = encrypt(device, payload, &self.rng).map_err(Sent::Failed)?;
        let audience =
            origin(&device.endpoint).ok_or_else(|| Sent::Failed("a bad endpoint".into()))?;
        let token = self.token(&audience).map_err(Sent::Failed)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(SEND_LIMIT))
            .http_status_as_error(false)
            .build()
            .into();
        let response = agent
            .post(&device.endpoint)
            .header("TTL", TTL)
            .header("Urgency", "high")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header(
                "Authorization",
                &format!("vapid t={token}, k={}", self.public_key()),
            )
            .send(&body[..])
            .map_err(|err| Sent::Failed(format!("{audience}: {err}")))?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            404 | 410 => Err(Sent::Gone),
            status => Err(Sent::Failed(format!("{audience} said {status}"))),
        }
    }

    /// The VAPID token for a push service at `audience`: a JWT signed with
    /// the server's key, good for twelve hours.
    fn token(&self, audience: &str) -> Result<String, String> {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 12 * 3600;
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(
            json!({ "aud": audience, "exp": expires, "sub": self.contact })
                .to_string()
                .as_bytes(),
        );
        let signed = format!("{header}.{claims}");
        let signature = self
            .vapid
            .sign(&self.rng, signed.as_bytes())
            .map_err(|_| "could not sign the VAPID token".to_string())?;
        Ok(format!("{signed}.{}", B64.encode(signature.as_ref())))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, all: &[Subscription]) -> Result<(), String> {
        let text = serde_json::to_string_pretty(all).unwrap_or_default();
        write_private(&self.dir.join("subscriptions.json"), text.as_bytes())
    }
}

enum Sent {
    /// The push service no longer knows the device.
    Gone,
    Failed(String),
}

/// Writes a file only its owner can read: it holds a private key, or where
/// to reach the owner's phone.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let partial = path.with_extension("partial");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// `https://host[:port]` of a URL.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// A length for `ring`'s HKDF output.
struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[&[u8]], len: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0; len];
    prk.expand(info, Len(len))
        .and_then(|okm| okm.fill(&mut out))
        .map_err(|_| "key derivation failed".to_string())?;
    Ok(out)
}

/// The record size the body says it uses; one record holds any message
/// treetop sends.
const RECORD: u32 = 4096;

/// `payload` encrypted for `device` as RFC 8291's `aes128gcm`: a fresh key
/// pair and salt each time, the content key and nonce from both sides'
/// keys and the device's secret, and the server's public key in the header
/// so the device can do the same.
pub fn encrypt(
    device: &Subscription,
    payload: &[u8],
    rng: &dyn SecureRandom,
) -> Result<Vec<u8>, String> {
    let device_key = B64
        .decode(device.keys.p256dh.trim_end_matches('='))
        .map_err(|_| "a subscription key that is not base64url")?;
    let auth = B64
        .decode(device.keys.auth.trim_end_matches('='))
        .map_err(|_| "a subscription secret that is not base64url")?;
    let ours = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, rng)
        .map_err(|_| "could not make a key")?;
    let our_public = ours
        .compute_public_key()
        .map_err(|_| "could not make a key")?;
    let our_public = our_public.as_ref().to_vec();
    let their_public = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &device_key);
    let shared = agreement::agree_ephemeral(ours, &their_public, |secret| secret.to_vec())
        .map_err(|_| "the subscription's key is not a P-256 key")?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| "no randomness")?;
    seal(&shared, &auth, &device_key, &our_public, &salt, payload)
}

/// The body for a message, from the keys both sides agreed on.
fn seal(
    shared: &[u8],
    auth: &[u8],
    device_key: &[u8],
    our_public: &[u8],
    salt: &[u8; 16],
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    let (key, nonce) = keys(shared, auth, device_key, our_public, salt)?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &key).map_err(|_| "a bad content key")?,
    );
    let nonce = aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| "a bad nonce")?;
    // One record, the last: the message, then the delimiter 0x02.
    let mut record = payload.to_vec();
    record.push(2);
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record)
        .map_err(|_| "encryption failed")?;
    let mut body = Vec::with_capacity(16 + 4 + 1 + our_public.len() + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD.to_be_bytes());
    body.push(u8::try_from(our_public.len()).map_err(|_| "a bad key")?);
    body.extend_from_slice(our_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// The content key and nonce, as RFC 8291 section 3.4 and RFC 8188 derive
/// them.
fn keys(
    shared: &[u8],
    auth: &[u8],
    device_key: &[u8],
    our_public: &[u8],
    salt: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(shared);
    let ikm = expand(&prk_key, &[b"WebPush: info\0", device_key, our_public], 32)?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let key = expand(&prk, &[b"Content-Encoding: aes128gcm\0"], 16)?;
    let nonce = expand(&prk, &[b"Content-Encoding: nonce\0"], 12)?;
    Ok((key, nonce))
}

/// One thing worth a buzz: the tree, what happened, and where tapping it
/// goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub url: String,
}

impl Notice {
    fn json(&self) -> Value {
        json!({ "title": self.title, "body": self.body, "url": self.url })
    }
}

/// `value` as it can sit in a query string: everything but letters,
/// digits and `-._~` percent-encoded.
fn query_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// What each tree looked like the last time: its agent's state and its
/// PR's stage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    trees: HashMap<String, (Option<Said>, Option<Stage>)>,
}

/// An agent's state as far as pushes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    NeedsYou,
    Done,
    Died,
    Other,
}

impl Seen {
    pub fn of(app: &App) -> Seen {
        let trees = app
            .trees
            .iter()
            .filter(|t| t.is_held() && !t.is_main)
            .map(|tree| {
                let waiting = tree.agents.iter().any(|a| {
                    a.status == Some(Status::Waiting) && !crate::assistant::is_assistant(&a.name)
                });
                let said = match app.says(tree).map(|i| i.state) {
                    Some(State::Blocked) => Some(Said::NeedsYou),
                    _ if waiting => Some(Said::NeedsYou),
                    Some(State::Done) => Some(Said::Done),
                    Some(State::Failed) => Some(Said::Died),
                    Some(_) => Some(Said::Other),
                    None => None,
                };
                (
                    tree.name.clone(),
                    (said, tree.ship.as_ref().map(|s| s.stage)),
                )
            })
            .collect();
        Seen { trees }
    }

    /// What changed from `self` to `now` that is worth a push: an agent
    /// coming to need you, finishing or dying, and a PR's checks failing or
    /// it merging. A tree seen for the first time says nothing, so a
    /// server starting up does not push the whole pool. Each opens its tree
    /// in `workspace`, its own, whichever one the phone last showed: tree 17
    /// of one repository is not tree 17 of another.
    pub fn notices(&self, now: &Seen, app: &App, workspace: &str) -> Vec<Notice> {
        let query = format!("?workspace={}", query_escape(workspace));
        let mut notices = Vec::new();
        for (name, (said, stage)) in &now.trees {
            let Some((was_said, was_stage)) = self.trees.get(name) else {
                continue;
            };
            let tree = app.trees.iter().find(|t| &t.name == name);
            let heading = tree
                .and_then(|t| {
                    let ticket = t.sidecar.as_ref().and_then(|s| s.ticket.clone());
                    title(t.branch.as_deref(), ticket.as_deref())
                })
                .map_or_else(|| name.clone(), |title| format!("{name} · {title}"));
            let chat = format!("/trees/{name}/chat{query}");
            let page = format!("/trees/{name}{query}");
            if said != was_said {
                let what = match said {
                    Some(Said::NeedsYou) => Some(("Needs you", &chat)),
                    Some(Said::Done) => Some(("Done", &chat)),
                    Some(Said::Died) => Some(("Its agent's process died", &page)),
                    _ => None,
                };
                if let Some((what, url)) = what {
                    notices.push(Notice {
                        title: heading.clone(),
                        body: what.into(),
                        url: url.clone(),
                    });
                }
            }
            if stage != was_stage {
                let what = match stage {
                    Some(Stage::CiFailed | Stage::Failed) => Some("Its PR's checks failed"),
                    Some(Stage::Merged) => Some("Its PR merged"),
                    _ => None,
                };
                if let Some(what) = what {
                    notices.push(Notice {
                        title: heading.clone(),
                        body: what.into(),
                        url: page.clone(),
                    });
                }
            }
        }
        notices.sort_by(|a, b| a.url.cmp(&b.url));
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, Source};
    use crate::reports::Report;
    use crate::serve::pool::tests::tree;
    use crate::ship::Ship;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("treetop-push-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_message_opens_on_the_phone_with_its_own_keys() {
        let rng = SystemRandom::new();
        // The phone's side: a key pair and a secret, as a browser makes them.
        let phone = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let phone_public = phone.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        let device = Subscription {
            endpoint: "https://web.push.apple.com/abc".into(),
            keys: SubscriptionKeys {
                p256dh: B64.encode(&phone_public),
                auth: B64.encode(auth),
            },
        };
        let payload =
            r#"{"title":"17 · END-123 fix-login","body":"Needs you","url":"/trees/17/chat"}"#
                .as_bytes();
        let body = encrypt(&device, payload, &rng).unwrap();

        // The header: salt, record size, the server's key.
        let (salt, rest) = body.split_at(16);
        assert_eq!(&rest[..4], &RECORD.to_be_bytes());
        let key_len = usize::from(rest[4]);
        assert_eq!(key_len, 65);
        let (server_public, sealed) = rest[5..].split_at(key_len);

        // Opened as the phone opens it.
        let shared = agreement::agree_ephemeral(
            phone,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, server_public),
            |secret| secret.to_vec(),
        )
        .unwrap();
        let (key, nonce) = keys(&shared, &auth, &phone_public, server_public, salt).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &key).unwrap());
        let mut sealed = sealed.to_vec();
        let opened = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
                aead::Aad::empty(),
                &mut sealed,
            )
            .unwrap();
        assert_eq!(opened.last(), Some(&2), "the last record's delimiter");
        assert_eq!(&opened[..opened.len() - 1], payload);
    }

    /// The opening above derives its keys with the same code that sealed the
    /// message, so it cannot catch a derivation no phone would share. This
    /// pins it to RFC 8291's worked example (Section 5, Appendix A).
    #[test]
    fn a_message_is_sealed_as_rfc_8291_s_example_is() {
        let b64 = |text: &str| B64.decode(text).unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = seal(
            &b64("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"),
            &b64("BTBZMqHH6r4Tts7J_aSIgg"),
            &b64(concat!(
                "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx",
                "aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            )),
            &b64(concat!(
                "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg",
                "Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8",
            )),
            &salt,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            B64.encode(body),
            // The body of the push in the RFC's Section 5.
            concat!(
                "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
                "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
                "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
            )
        );
    }

    #[test]
    fn the_push_service_can_check_the_token_against_the_server_s_key() {
        let dir = scratch("token");
        let push = Push::open(dir.clone(), "me@example.com").unwrap();
        let token = push.token("https://web.push.apple.com").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["sub"], "mailto:me@example.com");
        let key = B64.decode(push.public_key()).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, &key)
            .verify(
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &B64.decode(parts[2]).unwrap(),
            )
            .unwrap();
        // The same key the next time the server starts.
        let again = Push::open(dir.clone(), "me@example.com").unwrap();
        assert_eq!(again.public_key(), push.public_key());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn subscriptions_are_kept_once_each_and_only_for_the_owner_to_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("subscriptions");
        let push = Push::open(dir.clone(), "me").unwrap();
        let device = |endpoint: &str, auth: &str| Subscription {
            endpoint: endpoint.into(),
            keys: SubscriptionKeys {
                p256dh: "key".into(),
                auth: auth.into(),
            },
        };
        push.subscribe(device("https://a", "1")).unwrap();
        push.subscribe(device("https://a", "2")).unwrap();
        push.subscribe(device("https://b", "3")).unwrap();
        push.unsubscribe("https://b").unwrap();
        let kept = Push::open(dir.clone(), "me").unwrap();
        assert_eq!(*kept.lock(), [device("https://a", "2")]);
        for file in ["subscriptions.json", "vapid.pk8"] {
            let mode = fs::metadata(dir.join(file)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn report(job: &str, dir: &str, state: State) -> Report {
        Report {
            job: job.into(),
            name: job.into(),
            state,
            text: "what it said, which no push carries".into(),
            dir: Some(PathBuf::from(dir)),
            updated: UNIX_EPOCH + Duration::from_secs(100),
            steps: Vec::new(),
        }
    }

    fn ship(stage: Stage) -> Option<Box<Ship>> {
        Some(Box::new(Ship {
            branch: "b".into(),
            stage,
            label: String::new(),
            url: None,
            head: None,
            detail: Vec::new(),
            plugin: "github".into(),
        }))
    }

    #[test]
    fn a_change_worth_a_buzz_pushes_once_and_never_the_agent_s_words() {
        let mut app = App::new(None);
        let mut seventeen = tree("17", "me/END-123-fix-login");
        seventeen.agents = vec![Agent {
            name: "login".into(),
            pid: Some(1),
            status: Some(Status::Busy),
            source: Source::Plugin,
            is_background: true,
            attach: None,
            job: Some("j17".into()),
            plugin: None,
            active_at: None,
            context: None,
            spend: None,
        }];
        seventeen.ship = ship(Stage::Checks);
        app.set_trees(vec![seventeen, tree("14", "me/END-140-limits")]);
        app.apply_reports(vec![report("j17", "/pool/17", State::Working)]);
        let before = Seen::of(&app);

        // 17's agent asks something and its checks fail; 14 is unchanged.
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked)]);
        app.trees[0].ship = ship(Stage::CiFailed);
        let now = Seen::of(&app);
        let notices = before.notices(&now, &app, "my app");
        assert_eq!(
            notices,
            [
                Notice {
                    title: "17 · END-123 fix-login".into(),
                    body: "Needs you".into(),
                    url: "/trees/17/chat?workspace=my%20app".into(),
                },
                Notice {
                    title: "17 · END-123 fix-login".into(),
                    body: "Its PR's checks failed".into(),
                    url: "/trees/17?workspace=my%20app".into(),
                },
            ]
        );
        // Read again with nothing changed, nothing pushes.
        assert!(now.notices(&Seen::of(&app), &app, "app").is_empty());
        // A tree seen for the first time says nothing.
        assert!(Seen::default().notices(&now, &app, "app").is_empty());
        // Done, then merged.
        app.apply_reports(vec![report("j17", "/pool/17", State::Done)]);
        app.trees[0].ship = ship(Stage::Merged);
        let bodies: Vec<String> = now
            .notices(&Seen::of(&app), &app, "app")
            .into_iter()
            .map(|n| n.body)
            .collect();
        assert_eq!(bodies, ["Done", "Its PR merged"]);
    }

    #[test]
    fn a_push_goes_to_the_service_signed_and_a_gone_device_is_dropped() {
        let service = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = service.server_addr().to_ip().unwrap().port();
        let rng = SystemRandom::new();
        let phone = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let device = |path: &str| Subscription {
            endpoint: format!("http://127.0.0.1:{port}/{path}"),
            keys: SubscriptionKeys {
                p256dh: B64.encode(phone.compute_public_key().unwrap().as_ref()),
                auth: B64.encode([7u8; 16]),
            },
        };
        let dir = scratch("deliver");
        let push = Push::open(dir.clone(), "me@example.com").unwrap();
        push.subscribe(device("here")).unwrap();
        push.subscribe(device("gone")).unwrap();
        let answers = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let request = service.recv().unwrap();
                let header = |name: &'static str| {
                    request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv(name))
                        .map(|h| h.value.as_str().to_string())
                };
                seen.push((
                    request.url().to_string(),
                    header("Content-Encoding"),
                    header("TTL"),
                    header("Authorization").is_some_and(|a| a.starts_with("vapid t=")),
                ));
                let status = if request.url() == "/gone" { 410 } else { 201 };
                request.respond(tiny_http::Response::empty(status)).unwrap();
            }
            seen
        });
        push.send(&Notice {
            title: "17".into(),
            body: "Needs you".into(),
            url: "/trees/17/chat".into(),
        });
        let mut seen = answers.join().unwrap();
        seen.sort();
        let encoded = (Some("aes128gcm".to_string()), Some(TTL.to_string()), true);
        assert_eq!(
            seen,
            [
                (
                    "/gone".into(),
                    encoded.0.clone(),
                    encoded.1.clone(),
                    encoded.2
                ),
                ("/here".into(), encoded.0, encoded.1, encoded.2),
            ]
        );
        assert_eq!(*push.lock(), [device("here")]);
        // And still gone the next time the server starts.
        let kept = Push::open(dir.clone(), "me@example.com").unwrap();
        assert_eq!(*kept.lock(), [device("here")]);
        let _ = fs::remove_dir_all(&dir);
    }
}
