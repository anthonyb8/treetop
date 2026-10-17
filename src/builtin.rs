//! The plugins treetop ships with. Each answers the same requests and replies
//! as a plugin in the plugin directory, and `treetop plugin <name>` serves it
//! on stdin and stdout, so it can be run, tested and replaced like any other.
//! Inside treetop they are called in process, without a process per call.

use std::io::{self, Read};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agents::claude;
use crate::plugin::{Code, Error, Kind, PROTOCOL};

/// Every built-in by name, with its kind.
pub const KINDS: [(&str, Kind); 1] = [("claude", Kind::Agent)];

/// Answers `op` with `args` as the built-in `name`.
pub fn handle(name: &str, op: &str, args: &Value) -> Result<Value, Error> {
    match name {
        "claude" => claude::op(op, args),
        _ => Err(Error::new(
            Code::NotFound,
            format!("no built-in plugin {name}"),
        )),
    }
}

#[derive(Deserialize)]
struct Request {
    protocol: u32,
    op: String,
    #[serde(default)]
    args: Value,
}

/// The reply to one request's text.
fn answer(name: &str, text: &str) -> Value {
    let reply = serde_json::from_str::<Request>(text)
        .map_err(|err| Error::new(Code::Failed, format!("invalid request: {err}")))
        .and_then(|request| {
            if request.protocol != PROTOCOL {
                return Err(Error::new(
                    Code::Unsupported,
                    format!(
                        "asked in protocol {}, {name} speaks {PROTOCOL}",
                        request.protocol
                    ),
                ));
            }
            handle(name, &request.op, &request.args)
        });
    match reply {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

/// `treetop plugin <name>`: reads one request from stdin and prints the reply.
pub fn serve(name: &str) -> Result<()> {
    if !KINDS.iter().any(|(known, _)| *known == name) {
        let names: Vec<&str> = KINDS.iter().map(|(name, _)| *name).collect();
        bail!("no built-in plugin {name}; there are: {}", names.join(", "));
    }
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    println!("{}", answer(name, &text));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_itself_through_the_contract() {
        let reply = answer(
            "claude",
            r#"{"protocol":1,"op":"describe","config":{},"args":{}}"#,
        );
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"]["name"], "claude");
        assert_eq!(reply["result"]["kind"], "agent");
        assert_eq!(reply["result"]["protocol"], 1);
    }

    #[test]
    fn a_bad_request_or_protocol_is_an_error_reply() {
        let garbage = answer("claude", "hello");
        assert_eq!(garbage["ok"], false);
        assert_eq!(garbage["error"]["code"], "failed");
        let future = answer("claude", r#"{"protocol":2,"op":"describe"}"#);
        assert_eq!(future["error"]["code"], "unsupported");
        let unknown = answer("claude", r#"{"protocol":1,"op":"dance"}"#);
        assert_eq!(unknown["error"]["code"], "unsupported");
    }
}
