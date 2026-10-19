use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use futures::{Stream, StreamExt};
use tracing::{debug, warn};
use zbus::zvariant::Value;

use crate::ancs::protocol::CategoryId;
use crate::icons::Icons;
use crate::otp;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.kde.klipper.klipper",
    default_service = "org.kde.klipper",
    default_path = "/klipper"
)]
trait Klipper {
    #[zbus(name = "setClipboardContents")]
    fn set_clipboard_contents(&self, contents: &str) -> zbus::Result<()>;
}

const POSITIVE_KEY: &str = "positive";
const NEGATIVE_KEY: &str = "negative";
const COPY_CODE_KEY: &str = "copy-code";
const DISMISSED_BY_USER: u32 = 2;

pub enum Signal {
    Action { id: u32, key: String },
    Dismissed { id: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAction {
    Positive,
    Negative,
    Dismissed,
}

#[derive(Clone)]
pub struct Desktop {
    proxy: NotificationsProxy<'static>,
    timeout_ms: i32,
    icons: Arc<Icons>,
    clipboard: Option<KlipperProxy<'static>>,
}

impl Desktop {
    pub async fn connect(timeout_ms: i32, icons: Icons, copy_codes: bool) -> Result<Self> {
        let connection = zbus::Connection::session().await?;
        let proxy = NotificationsProxy::new(&connection).await?;
        let clipboard = if copy_codes {
            Some(KlipperProxy::new(&connection).await?)
        } else {
            None
        };
        Ok(Self { proxy, timeout_ms, icons: Arc::new(icons), clipboard })
    }

    pub fn for_device(&self, origin: String) -> DeviceNotifications {
        DeviceNotifications {
            desktop: self.clone(),
            origin,
            ids: HashMap::new(),
            acted: HashSet::new(),
            critical: HashSet::new(),
            codes: HashMap::new(),
        }
    }

    async fn signals(&self) -> Result<impl Stream<Item = Signal> + use<>> {
        let actions = self.proxy.receive_action_invoked().await?.filter_map(|signal| async move {
            let args = signal.args().ok()?;
            Some(Signal::Action { id: args.id, key: args.action_key })
        });
        let closed = self.proxy.receive_notification_closed().await?.filter_map(|signal| async move {
            let args = signal.args().ok()?;
            (args.reason == DISMISSED_BY_USER).then_some(Signal::Dismissed { id: args.id })
        });
        Ok(futures::stream::select(actions, closed))
    }
}

pub struct Content {
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub subtitle: String,
    pub message: String,
    pub category: CategoryId,
    pub silent: bool,
    pub important: bool,
    pub positive_action: Option<String>,
    pub negative_action: Option<String>,
}

pub struct DeviceNotifications {
    desktop: Desktop,
    origin: String,
    ids: HashMap<u32, u32>,
    acted: HashSet<u32>,
    critical: HashSet<u32>,
    codes: HashMap<u32, String>,
}

impl DeviceNotifications {
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub async fn signals(&self) -> Result<impl Stream<Item = Signal> + use<>> {
        self.desktop.signals().await
    }

    pub async fn resolve(&mut self, signal: Signal) -> Option<(u32, UserAction)> {
        let (id, action) = match signal {
            Signal::Action { id, key } => {
                let action = match key.as_str() {
                    POSITIVE_KEY => UserAction::Positive,
                    NEGATIVE_KEY => UserAction::Negative,
                    COPY_CODE_KEY => {
                        self.copy_code(id).await;
                        return None;
                    }
                    _ => return None,
                };
                (id, action)
            }
            Signal::Dismissed { id } => (id, UserAction::Dismissed),
        };
        let uid = self.uid_of(id)?;
        if action == UserAction::Dismissed {
            self.ids.remove(&uid);
            self.critical.remove(&uid);
            self.codes.remove(&uid);
            if self.acted.remove(&id) {
                return None;
            }
        } else if !self.acted.insert(id) {
            return None;
        }
        Some((uid, action))
    }

    fn uid_of(&self, id: u32) -> Option<u32> {
        self.ids.iter().find(|(_, desktop_id)| **desktop_id == id).map(|(uid, _)| *uid)
    }

    async fn copy_code(&mut self, id: u32) {
        let Some(uid) = self.uid_of(id) else {
            return;
        };
        let (Some(clipboard), Some(code)) = (&self.desktop.clipboard, self.codes.get(&uid)) else {
            return;
        };
        self.acted.insert(id);
        match clipboard.set_clipboard_contents(code).await {
            Ok(()) => debug!("ANCS {uid}: copied its code to the clipboard"),
            Err(err) => warn!("cannot copy the code to the clipboard: {err}"),
        }
    }

    pub fn is_shown(&self, uid: u32) -> bool {
        self.ids.contains_key(&uid)
    }

    pub async fn show(&mut self, uid: u32, content: &Content) {
        let summary = if content.title.is_empty() {
            &content.app_name
        } else {
            &content.title
        };
        let body = [content.subtitle.as_str(), content.message.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| escape_markup(s))
            .collect::<Vec<_>>()
            .join("\n");

        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("x-kde-appname", Value::from("ancs-linux"));
        hints.insert("x-kde-origin-name", Value::from(self.origin.as_str()));
        hints.insert("urgency", Value::U8(urgency(content)));
        if let Some(category) = category_hint(content.category) {
            hints.insert("category", Value::from(category));
        }
        if content.silent {
            hints.insert("suppress-sound", Value::Bool(true));
        }
        if content.category == CategoryId::IncomingCall {
            hints.insert("resident", Value::Bool(true));
        }

        let mut actions = Vec::new();
        if let Some(label) = &content.positive_action {
            actions.extend([POSITIVE_KEY, label.as_str()]);
        }
        if let Some(label) = &content.negative_action {
            actions.extend([NEGATIVE_KEY, label.as_str()]);
        }
        let code = self.desktop.clipboard.as_ref().and_then(|_| {
            otp::find_code(&content.app_id, &content.title, &content.subtitle, &content.message)
        });
        let copy_label = code.as_ref().map(|code| format!("Copy {code}"));
        if let Some(label) = &copy_label {
            actions.extend([COPY_CODE_KEY, label.as_str()]);
        }

        let icon = self.desktop.icons.icon_for(&content.app_id).await;
        let replaces = self.ids.get(&uid).copied().unwrap_or(0);
        let critical = urgency(content) == 2;
        let timeout = if critical { 0 } else { self.desktop.timeout_ms };
        match self
            .desktop
            .proxy
            .notify(
                &content.app_name,
                replaces,
                &icon,
                summary,
                &body,
                &actions,
                hints,
                timeout,
            )
            .await
        {
            Ok(id) => {
                debug!("ANCS {uid} shown as desktop notification {id}");
                self.ids.insert(uid, id);
                if critical {
                    self.critical.insert(uid);
                } else {
                    self.critical.remove(&uid);
                }
                match code {
                    Some(code) => self.codes.insert(uid, code),
                    None => self.codes.remove(&uid),
                };
            }
            Err(err) => warn!("Notify failed: {err}"),
        }
    }

    pub async fn close(&mut self, uid: u32) {
        let Some(id) = self.ids.remove(&uid) else {
            return;
        };
        self.acted.remove(&id);
        self.critical.remove(&uid);
        self.codes.remove(&uid);
        if let Err(err) = self.desktop.proxy.close_notification(id).await
        {
            debug!("CloseNotification {id} failed: {err}");
        }
    }

    pub async fn release(&mut self) {
        for uid in self.critical.iter().copied().collect::<Vec<_>>() {
            self.close(uid).await;
        }
        self.ids.clear();
        self.acted.clear();
        self.codes.clear();
    }
}

/// 0 low, 1 normal, 2 critical
fn urgency(content: &Content) -> u8 {
    if content.category == CategoryId::IncomingCall || content.important {
        2
    } else if content.silent {
        0
    } else {
        1
    }
}

fn category_hint(category: CategoryId) -> Option<&'static str> {
    match category {
        CategoryId::IncomingCall => Some("call.incoming"),
        CategoryId::MissedCall => Some("call.unanswered"),
        CategoryId::Voicemail => Some("call"),
        CategoryId::Social => Some("im.received"),
        CategoryId::Email => Some("email.arrived"),
        _ => None,
    }
}

fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(escape_markup("a <b> & c"), "a &lt;b&gt; &amp; c");
    }
}
