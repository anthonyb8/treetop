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

use crate::app::App;
use crate::inbox::Kind;

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

/// How long something waits on you before the phone is pushed, and how
/// long you must have been away from tmux by then.
pub const FIRST: Duration = Duration::from_secs(5 * 60);

/// How long it waits before the one reminder; after that, no more.
pub const AGAIN: Duration = Duration::from_secs(30 * 60);

/// How long something can drop out of what waits, as an agent read as
/// busy for a moment, and come back as the same thing, not pushed again.
const FORGET: Duration = Duration::from_secs(2 * 60);

/// Pushes it has had: none, the first, or the reminder, after which none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pushes {
    None,
    First,
    Spent,
}

/// What has been pushed about one waiting thing.
#[derive(Debug, Clone, Copy)]
struct Pushed {
    pushes: Pushes,
    /// When the last push went, so the reminder comes AGAIN - FIRST after
    /// it however late the first was.
    at: Option<SystemTime>,
    /// When it was last seen waiting, for FORGET.
    seen: SystemTime,
}

/// When the phone is pushed about what waits in this workspace's trees:
/// once it has waited FIRST while you are away from tmux, again at AGAIN
/// and not sooner than AGAIN - FIRST after the first, and never after.
/// Answered at the desk before then, it is never pushed.
#[derive(Debug, Default)]
pub struct Schedule {
    /// What has been pushed, by what it is about; None until the first
    /// look, which pushes nothing, so a server starting up does not buzz
    /// the phone with the whole pool.
    pushed: Option<HashMap<crate::inbox::About, Pushed>>,
}

impl Schedule {
    /// The pushes due as of `now`, with you `idle` in tmux for so long, or
    /// None where tmux cannot say, which counts as away. Each opens its
    /// tree in `workspace`, its own, whichever one the phone last showed:
    /// tree 17 of one repository is not tree 17 of another. A push names
    /// the tree and what it waits for, never what the agent wrote. Another
    /// workspace's are left to that workspace's own pool.
    pub fn due(
        &mut self,
        app: &App,
        idle: Option<Duration>,
        now: SystemTime,
        workspace: &str,
    ) -> Vec<Notice> {
        let waiting: Vec<crate::inbox::Entry> = crate::inbox::needs_action(app)
            .into_iter()
            .filter(|e| e.away.is_none())
            .collect();
        let Some(pushed) = self.pushed.as_mut() else {
            let spent = |_| Pushed {
                pushes: Pushes::Spent,
                at: None,
                seen: now,
            };
            self.pushed = Some(waiting.iter().map(|e| (e.about(), spent(()))).collect());
            return Vec::new();
        };
        pushed.retain(|about, p| {
            waiting.iter().any(|e| e.about() == *about)
                || now.duration_since(p.seen).unwrap_or_default() < FORGET
        });
        let is_away = idle.is_none_or(|idle| idle >= FIRST);
        let query = format!("?workspace={}", query_escape(workspace));
        let mut notices = Vec::new();
        for entry in &waiting {
            let p = pushed.entry(entry.about()).or_insert(Pushed {
                pushes: Pushes::None,
                at: None,
                seen: now,
            });
            p.seen = now;
            let since = |at: Option<SystemTime>| {
                at.and_then(|at| now.duration_since(at).ok())
                    .unwrap_or_default()
            };
            let waited = since(entry.at);
            let is_due = match p.pushes {
                Pushes::None => waited >= FIRST,
                Pushes::First => waited >= AGAIN && since(p.at) >= AGAIN - FIRST,
                Pushes::Spent => false,
            };
            if !is_due || !is_away {
                continue;
            }
            p.pushes = if p.pushes == Pushes::None {
                Pushes::First
            } else {
                Pushes::Spent
            };
            p.at = Some(now);
            let title = entry.title.as_ref().map_or_else(
                || entry.tree.clone(),
                |title| format!("{} · {title}", entry.tree),
            );
            let (what, page) = match entry.kind {
                Kind::NeedsYou => ("Needs you", "/chat"),
                Kind::Failed => ("Its PR's checks failed", ""),
                _ => ("Its agent's process died", ""),
            };
            let body = if p.pushes == Pushes::First {
                what.to_string()
            } else {
                format!("Still: {what}, {}m", waited.as_secs() / 60)
            };
            notices.push(Notice {
                title,
                body,
                url: format!("/trees/{}{page}{query}", entry.tree),
            });
        }
        notices.sort_by(|a, b| a.url.cmp(&b.url));
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, Source, Status};
    use crate::reports::{Report, State};
    use crate::serve::pool::tests::tree;
    use crate::ship::{Ship, Stage};
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

    fn report(job: &str, dir: &str, state: State, updated: SystemTime) -> Report {
        Report {
            job: job.into(),
            name: job.into(),
            state,
            text: "what it said, which no push carries".into(),
            dir: Some(PathBuf::from(dir)),
            updated,
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

    fn agent(job: &str) -> Agent {
        Agent {
            name: job.into(),
            pid: Some(1),
            status: Some(Status::Busy),
            source: Source::Plugin,
            is_background: true,
            attach: None,
            job: Some(job.into()),
            plugin: None,
            active_at: None,
            context: None,
            spend: None,
            label: None,
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    /// A pool of trees 17 and 14, their agents working and 17's PR under
    /// checks, read as of `start`.
    fn pool(start: SystemTime) -> App {
        let mut app = App::new(None);
        let mut seventeen = tree("17", "me/END-123-fix-login");
        seventeen.agents = vec![agent("j17")];
        seventeen.ship = ship(Stage::Checks);
        let mut fourteen = tree("14", "me/END-140-limits");
        fourteen.agents = vec![agent("j14")];
        app.set_trees(vec![seventeen, fourteen]);
        app.is_agents_read = true;
        app.apply_reports(vec![
            report("j17", "/pool/17", State::Working, start),
            report("j14", "/pool/14", State::Working, start),
        ]);
        app
    }

    #[test]
    fn an_ask_pushes_once_you_are_away_five_minutes_and_again_at_thirty_then_never() {
        let start = SystemTime::now();
        let at = |minutes: u32| start + MIN * minutes;
        let mut app = pool(start);
        let mut schedule = Schedule::default();
        let away = Some(10 * MIN);
        assert!(schedule.due(&app, away, at(0), "my app").is_empty());
        // 17's agent asks.
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked, start)]);
        app.watch_waiting();
        let bodies = |schedule: &mut Schedule, idle, minutes| -> Vec<String> {
            schedule
                .due(&app, idle, at(minutes), "my app")
                .into_iter()
                .map(|n| n.body)
                .collect()
        };
        assert!(bodies(&mut schedule, away, 4).is_empty(), "too soon");
        assert!(
            bodies(&mut schedule, Some(MIN), 6).is_empty(),
            "you are at the desk"
        );
        let first = schedule.due(&app, away, at(7), "my app");
        assert_eq!(
            first,
            [Notice {
                title: "17 · END-123 fix-login".into(),
                body: "Needs you".into(),
                url: "/trees/17/chat?workspace=my%20app".into(),
            }],
            "never what the agent wrote"
        );
        assert!(bodies(&mut schedule, away, 20).is_empty(), "once");
        assert!(
            bodies(&mut schedule, away, 31).is_empty(),
            "25 minutes after the first"
        );
        assert_eq!(bodies(&mut schedule, away, 32), ["Still: Needs you, 32m"]);
        assert!(bodies(&mut schedule, away, 120).is_empty(), "then never");
    }

    #[test]
    fn the_reminder_comes_well_after_the_first_push_however_late_that_was() {
        let start = SystemTime::now();
        let at = |minutes: u32| start + MIN * minutes;
        let mut app = pool(start);
        let mut schedule = Schedule::default();
        schedule.due(&app, Some(MIN), at(0), "app");
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked, start)]);
        app.watch_waiting();
        // At the desk until minute 35, then away.
        assert!(schedule.due(&app, Some(MIN), at(34), "app").is_empty());
        let away = Some(10 * MIN);
        assert_eq!(schedule.due(&app, away, at(35), "app").len(), 1);
        assert!(
            schedule.due(&app, away, at(36), "app").is_empty(),
            "not a second buzz straight after the first"
        );
        assert_eq!(
            schedule.due(&app, away, at(60), "app").len(),
            1,
            "25 minutes on"
        );
    }

    #[test]
    fn an_ask_that_flickers_out_and_back_is_the_one_ask() {
        let start = SystemTime::now();
        let at = |minutes: u32| start + MIN * minutes;
        let mut app = pool(start);
        let mut schedule = Schedule::default();
        schedule.due(&app, None, at(0), "app");
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked, start)]);
        app.watch_waiting();
        assert_eq!(schedule.due(&app, None, at(6), "app").len(), 1);
        // For a moment its agent reads as busy, then asks again.
        app.apply_reports(vec![report("j17", "/pool/17", State::Working, start)]);
        schedule.due(&app, None, at(7), "app");
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked, start)]);
        app.watch_waiting();
        assert!(
            schedule.due(&app, None, at(8), "app").is_empty(),
            "no second first push"
        );
    }

    #[test]
    fn answered_at_the_desk_in_time_and_what_finishes_or_merges_never_push() {
        let start = SystemTime::now();
        let at = |minutes: u32| start + MIN * minutes;
        let mut app = pool(start);
        let mut schedule = Schedule::default();
        // As the server does each round: what waits is looked at, then due.
        // With no tmux to ask, idle counts as away.
        let mut due = |app: &mut App, minutes| {
            app.watch_waiting();
            schedule.due(app, None, at(minutes), "app")
        };
        assert!(due(&mut app, 0).is_empty());
        app.apply_reports(vec![report("j14", "/pool/14", State::Blocked, start)]);
        assert!(due(&mut app, 3).is_empty());
        // Answered at the desk before five minutes are up.
        app.apply_reports(vec![report("j14", "/pool/14", State::Working, start)]);
        // 14's turn then ends, its session idle: done, which never pushes.
        let fourteen = app.trees.iter_mut().find(|t| t.name == "14").unwrap();
        fourteen.agents[0].status = Some(Status::Idle);
        fourteen.agents[0].active_at = Some(
            at(4)
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        assert!(due(&mut app, 10).is_empty(), "a finished turn never pushes");
        // 17 finishes and its PR merges, each seen at once and long ago by
        // the time it is asked about.
        app.apply_reports(vec![report("j17", "/pool/17", State::Done, start)]);
        app.apply_ship(Ok(crate::ship::Answer {
            ships: [(PathBuf::from("/pool/17"), *ship(Stage::Merged).unwrap())].into(),
            failures: Vec::new(),
        }));
        assert!(due(&mut app, 10).is_empty());
        assert!(due(&mut app, 60).is_empty());
    }

    #[test]
    fn failed_checks_and_a_dead_agent_push_to_their_tree_but_not_what_waited_as_the_server_started()
    {
        let start = SystemTime::now();
        let at = |minutes: u32| start + MIN * minutes;
        let mut app = pool(start);
        // Already waiting as the server starts: 17's agent asking.
        app.apply_reports(vec![report("j17", "/pool/17", State::Blocked, start)]);
        let mut schedule = Schedule::default();
        app.watch_waiting();
        assert!(schedule.due(&app, None, at(0), "app").is_empty());
        // Then 14's agent dies and 17's checks fail.
        app.apply_reports(vec![
            report("j17", "/pool/17", State::Blocked, start),
            report("j14", "/pool/14", State::Failed, start),
        ]);
        app.apply_ship(Ok(crate::ship::Answer {
            ships: [(PathBuf::from("/pool/17"), *ship(Stage::CiFailed).unwrap())].into(),
            failures: Vec::new(),
        }));
        app.watch_waiting();
        let pushed: Vec<(String, String)> = schedule
            .due(&app, None, at(6), "app")
            .into_iter()
            .map(|n| (n.url, n.body))
            .collect();
        assert_eq!(
            pushed,
            [
                (
                    "/trees/14?workspace=app".into(),
                    "Its agent's process died".into()
                ),
                (
                    "/trees/17?workspace=app".into(),
                    "Its PR's checks failed".into()
                ),
            ],
            "17's ask, waiting from the start, never pushes"
        );
        let reminded: Vec<String> = schedule
            .due(&app, None, at(60), "app")
            .into_iter()
            .map(|n| n.url)
            .collect();
        assert_eq!(
            reminded,
            ["/trees/14?workspace=app", "/trees/17?workspace=app"],
            "nor reminds"
        );
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
