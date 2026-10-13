//! Finding and closing the frozen pages of a session: pages whose main thread never yields, e.g. to
//! an endless chain of microtasks.
//!
//! `ChromeDriver` runs the commands of a session one after the other. Once one of them waits for a
//! frozen page, every later one waits behind it, including CDP commands sent through
//! `ChromeDriver` and quitting the session. The browser process stays responsive, though, so this
//! module talks to its `DevTools` endpoint directly, over one `WebSocket` to the browser in flat
//! mode (found through the session's `goog:chromeOptions.debuggerAddress`): it lists the pages and
//! frames of the session, attaches to each, and one that doesn't answer a `Runtime.evaluate` in
//! time is frozen. Closing it ends the commands waiting for it.
//!
//! thirtyfour's `CdpSession` finds the same endpoint, but attaches to one page only, and can
//! neither address other pages nor the browser itself.

use std::{
    collections::HashMap,
    fmt,
    time::{Duration, Instant},
};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use thirtyfour::WebDriver;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

/// How long a page has to answer before it counts as frozen. A responsive page answers within
/// milliseconds.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the browser (not a page) may take to answer a request.
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long checking the pages of a session may take overall.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Chrome's error for a target that closed meanwhile.
const NO_TARGET: &str = "No target with given id found";

/// What checking the pages of a session found.
pub(crate) enum PageCheck {
    /// The pages were checked: the frozen ones (closed, unless closing them failed), and why some
    /// could not be checked.
    Checked {
        frozen: Vec<FrozenPage>,
        unchecked: Vec<String>,
    },
    /// A dialog (`alert`, `confirm`, `prompt`, `beforeunload`) is open, with this text. Its page
    /// waits for it, but the session doesn't, so nothing was closed.
    Dialog(String),
    /// The pages could not be checked, for this reason.
    Unknown(String),
}

/// A page or frame that didn't answer within [`RESPONSE_TIMEOUT`].
pub(crate) struct FrozenPage {
    url: String,
    /// Whether it is a frame of a page (an out-of-process `iframe`), not a page.
    frame: bool,
    /// Why closing it failed.
    close_error: Option<String>,
}

impl fmt::Display for PageCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checked { frozen, unchecked } if frozen.is_empty() => {
                if unchecked.is_empty() {
                    f.write_str("Its pages respond: none of them is frozen.")
                } else {
                    f.write_str("Its pages that could be checked respond.")?;
                    write_unchecked(f, unchecked)
                }
            }
            Self::Checked { frozen, unchecked } => {
                let (pages, threads, closed) = match frozen.as_slice() {
                    [_] => (
                        "its page".to_owned(),
                        "its main thread never yields",
                        "it was",
                    ),
                    _ => (
                        format!("{} of its pages", frozen.len()),
                        "their main threads never yield",
                        "they were",
                    ),
                };
                write!(
                    f,
                    "Frozen: {pages} didn't respond within {}s ({threads}, e.g. to an endless \
                     chain of microtasks)",
                    RESPONSE_TIMEOUT.as_secs(),
                )?;
                if frozen.iter().all(|page| page.close_error.is_none()) {
                    write!(f, ", so {closed} closed:")?;
                } else {
                    f.write_str(":")?;
                }
                for page in frozen {
                    write!(f, "\n  {}", page.url)?;
                    if page.frame {
                        f.write_str(" (a frame)")?;
                    }
                    if let Some(error) = &page.close_error {
                        write!(f, " (could not be closed: {error})")?;
                    }
                }
                write_unchecked(f, unchecked)
            }
            Self::Dialog(text) => write!(f, "A dialog is open, which its page waits for: {text}"),
            Self::Unknown(reason) => write!(f, "Its pages could not be checked: {reason}"),
        }
    }
}

fn write_unchecked(f: &mut fmt::Formatter<'_>, unchecked: &[String]) -> fmt::Result {
    if unchecked.is_empty() {
        return Ok(());
    }
    f.write_str("\nSome could not be checked:")?;
    for reason in unchecked {
        write!(f, "\n  {reason}")?;
    }
    Ok(())
}

// Reports are printed with `Debug` too (a `main` or test returning `Err`): attachments read the same.
impl fmt::Debug for PageCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Check every page of `session` and close the frozen ones, unless a dialog is open.
pub(crate) async fn close_frozen_pages(session: &WebDriver) -> PageCheck {
    tokio::time::timeout(CHECK_TIMEOUT, try_close_frozen_pages(session))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "checking them took longer than {}s",
                CHECK_TIMEOUT.as_secs()
            ))
        })
        .unwrap_or_else(PageCheck::Unknown)
}

async fn try_close_frozen_pages(session: &WebDriver) -> Result<PageCheck, String> {
    let mut browser = Browser::connect(session).await?;
    let targets = browser.call("Target.getTargets", json!({})).await?;
    let targets: Vec<Target> = targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .ok_or("the list of pages has no `targetInfos`")?
        .iter()
        .filter_map(Target::from_info)
        .collect();
    let mut unchecked = Vec::new();
    let unanswered = browser.unanswered(&targets, &mut unchecked);
    // A dialog blocks its page as well, but not the session: `ChromeDriver` answers.
    let (unanswered, dialog) = tokio::join!(unanswered, open_dialog(session));
    let unanswered = unanswered?;
    if let Some(text) = dialog {
        return Ok(PageCheck::Dialog(text));
    }
    let frozen = browser.close(&unanswered).await?;
    Ok(PageCheck::Checked { frozen, unchecked })
}

/// The text of the session's open dialog, if `ChromeDriver` answers within [`RESPONSE_TIMEOUT`].
async fn open_dialog(session: &WebDriver) -> Option<String> {
    tokio::time::timeout(RESPONSE_TIMEOUT, session.get_alert_text())
        .await
        .ok()?
        .ok()
}

/// A page or out-of-process frame of the browser.
struct Target {
    id: String,
    url: String,
    frame: bool,
}

impl Target {
    fn from_info(info: &Value) -> Option<Self> {
        let field = |name: &str| info.get(name).and_then(Value::as_str);
        let frame = match field("type")? {
            "page" => false,
            "iframe" => true,
            _ => return None,
        };
        Some(Self {
            id: field("targetId")?.to_owned(),
            url: field("url").unwrap_or_default().to_owned(),
            frame,
        })
    }
}

/// The reply to a request: its result, or the error's message.
type Reply = Result<Value, String>;

/// A connection to the browser's `DevTools` endpoint, in flat mode: requests to a page carry the
/// id of the session attached to it.
struct Browser {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Browser {
    async fn connect(session: &WebDriver) -> Result<Self, String> {
        let address = session
            .handle()
            .capabilities()
            .get("goog:chromeOptions")
            .and_then(|options| options.get("debuggerAddress"))
            .and_then(Value::as_str)
            .ok_or("the session has no `goog:chromeOptions.debuggerAddress`")?
            .to_owned();
        let url = browser_url(&address).await?;
        let (socket, _) = tokio::time::timeout(
            ENDPOINT_TIMEOUT,
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await
        .map_err(|_| format!("connecting to {url} timed out"))?
        .map_err(|error| format!("connecting to {url} failed: {error}"))?;
        Ok(Self { socket, next_id: 0 })
    }

    /// Send `method` to the page attached as `session` (`None`: to the browser), and return the
    /// request's id.
    async fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<u64, String> {
        self.next_id += 1;
        let mut request = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = session {
            request["sessionId"] = Value::from(session);
        }
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .map_err(|error| format!("sending `{method}` failed: {error}"))?;
        Ok(self.next_id)
    }

    /// The replies to the requests `ids` that arrive within `limit`. A request without one is
    /// missing.
    async fn replies(
        &mut self,
        ids: &[u64],
        limit: Duration,
    ) -> Result<HashMap<u64, Reply>, String> {
        let deadline = Instant::now() + limit;
        let mut replies = HashMap::new();
        while replies.len() < ids.len() {
            let Ok(message) = tokio::time::timeout_at(deadline.into(), self.socket.next()).await
            else {
                break;
            };
            let text = match message {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(_)) => continue,
                Some(Err(error)) => {
                    return Err(format!("the `DevTools` connection failed: {error}"));
                }
                None => return Err("the `DevTools` connection closed".to_owned()),
            };
            let Ok(reply) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            // Events have no id.
            let Some(id) = reply
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| ids.contains(id))
            else {
                continue;
            };
            let reply = match reply.get("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned()),
                None => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            };
            replies.insert(id, reply);
        }
        Ok(replies)
    }

    /// Send `method` to the browser and return its result.
    async fn call(&mut self, method: &str, params: Value) -> Reply {
        let id = self.send(method, params, None).await?;
        self.replies(&[id], ENDPOINT_TIMEOUT)
            .await?
            .remove(&id)
            .unwrap_or_else(|| {
                Err(format!(
                    "`{method}` got no answer within {}s",
                    ENDPOINT_TIMEOUT.as_secs()
                ))
            })
            .map_err(|error| format!("`{method}` failed: {error}"))
    }

    /// The targets of `targets` that don't evaluate a script within [`RESPONSE_TIMEOUT`]. Why one
    /// could not be checked is added to `unchecked`. A target that closed meanwhile doesn't block
    /// the session and is left out.
    async fn unanswered<'t>(
        &mut self,
        targets: &'t [Target],
        unchecked: &mut Vec<String>,
    ) -> Result<Vec<&'t Target>, String> {
        let mut attach_ids = Vec::new();
        for target in targets {
            let params = json!({ "targetId": target.id, "flatten": true });
            attach_ids.push(self.send("Target.attachToTarget", params, None).await?);
        }
        let mut sessions = self.replies(&attach_ids, ENDPOINT_TIMEOUT).await?;
        let mut probes = Vec::new();
        for (target, id) in targets.iter().zip(attach_ids) {
            let session = match sessions.remove(&id) {
                Some(Ok(reply)) => reply
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                Some(Err(error)) if error == NO_TARGET => continue,
                Some(Err(error)) => {
                    unchecked.push(format!("{} could not be attached to: {error}", target.url));
                    continue;
                }
                None => None,
            };
            let Some(session) = session else {
                unchecked.push(format!("{} could not be attached to in time", target.url));
                continue;
            };
            let params = json!({ "expression": "0" });
            let probe = self
                .send("Runtime.evaluate", params, Some(&session))
                .await?;
            probes.push((target, probe));
        }
        let ids: Vec<u64> = probes.iter().map(|(_, id)| *id).collect();
        // Any reply counts, an error as well: something answered.
        let answered = self.replies(&ids, RESPONSE_TIMEOUT).await?;
        Ok(probes
            .into_iter()
            .filter(|(_, id)| !answered.contains_key(id))
            .map(|(target, _)| target)
            .collect())
    }

    /// Close `targets`, all at once.
    async fn close(&mut self, targets: &[&Target]) -> Result<Vec<FrozenPage>, String> {
        let mut ids = Vec::new();
        for target in targets {
            let params = json!({ "targetId": target.id });
            ids.push(self.send("Target.closeTarget", params, None).await?);
        }
        let mut replies = self.replies(&ids, ENDPOINT_TIMEOUT).await?;
        Ok(targets
            .iter()
            .zip(ids)
            .map(|(target, id)| {
                let close_error = match replies.remove(&id) {
                    Some(Ok(reply)) if reply.get("success") == Some(&Value::Bool(false)) => {
                        Some("the browser refused".to_owned())
                    }
                    Some(Ok(_)) => None,
                    Some(Err(error)) if error == NO_TARGET => None,
                    Some(Err(error)) => Some(error),
                    None => Some(format!("no answer within {}s", ENDPOINT_TIMEOUT.as_secs())),
                };
                FrozenPage {
                    url: target.url.clone(),
                    frame: target.frame,
                    close_error,
                }
            })
            .collect())
    }
}

/// The `WebSocket` URL of the browser listening on `address`.
async fn browser_url(address: &str) -> Result<String, String> {
    let url = format!("http://{address}/json/version");
    // The endpoint is local: a proxy configured for the environment must not be asked for it.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(ENDPOINT_TIMEOUT)
        .build()
        .map_err(|error| format!("building an HTTP client failed: {error}"))?;
    let version: Value = async {
        let response = client.get(&url).send().await?.error_for_status()?;
        response.text().await
    }
    .await
    .map_err(|error| format!("the request {url} failed: {error}"))
    .and_then(|body| {
        serde_json::from_str(&body).map_err(|error| format!("{url} returned no JSON: {error}"))
    })?;
    version
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{url} has no `webSocketDebuggerUrl`"))
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    fn frozen(url: &str, frame: bool, close_error: Option<&str>) -> FrozenPage {
        FrozenPage {
            url: url.to_owned(),
            frame,
            close_error: close_error.map(str::to_owned),
        }
    }

    #[test]
    fn one_closed_page_reads_singular() {
        let check = PageCheck::Checked {
            frozen: vec![frozen("http://a/", false, None)],
            unchecked: Vec::new(),
        };
        assert_that!(check.to_string()).is_equal_to(
            "Frozen: its page didn't respond within 2s (its main thread never yields, e.g. to an \
             endless chain of microtasks), so it was closed:\n  http://a/"
                .to_owned(),
        );
    }

    #[test]
    fn several_pages_read_plural_and_name_frames_and_failures() {
        let check = PageCheck::Checked {
            frozen: vec![
                frozen("http://a/", false, None),
                frozen("http://b/", true, Some("the browser refused")),
            ],
            unchecked: vec!["http://c/ could not be attached to in time".to_owned()],
        };
        assert_that!(check.to_string()).is_equal_to(
            "Frozen: 2 of its pages didn't respond within 2s (their main threads never yield, \
             e.g. to an endless chain of microtasks):\n  http://a/\n  http://b/ (a frame) (could \
             not be closed: the browser refused)\nSome could not be checked:\n  http://c/ could \
             not be attached to in time"
                .to_owned(),
        );
    }

    #[test]
    fn responsive_pages_say_so() {
        let check = PageCheck::Checked {
            frozen: Vec::new(),
            unchecked: Vec::new(),
        };
        assert_that!(check.to_string())
            .is_equal_to("Its pages respond: none of them is frozen.".to_owned());
    }

    #[test]
    fn only_pages_and_frames_are_checked() {
        let target = |kind: &str| json!({ "targetId": "1", "type": kind, "url": "http://a/" });
        assert_that!(Target::from_info(&target("page")).map(|target| target.frame))
            .is_equal_to(Some(false));
        assert_that!(Target::from_info(&target("iframe")).map(|target| target.frame))
            .is_equal_to(Some(true));
        assert_that!(Target::from_info(&target("service_worker")).is_none()).is_true();
    }
}
