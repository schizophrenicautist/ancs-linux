use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use bluer::gatt::WriteOp;
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use futures::{Stream, StreamExt};
use tokio::time::{Instant, sleep, sleep_until};
use tracing::{debug, info, warn};

use super::assembler::{Assembler, Progress, Response};
use super::protocol::{
    self, ActionId, CategoryId, CommandId, EventId, SourceEvent, att_error, event_flags,
    notification_attr,
};
use crate::desktop::{Content, DeviceNotifications, UserAction};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_QUEUED_REQUESTS: usize = 64;
const SUBSCRIBE_RETRY: Duration = Duration::from_secs(3);
const MAX_AGE: chrono::TimeDelta = chrono::TimeDelta::minutes(2);

const REQUESTED_ATTRIBUTES: &[u8] = &[
    notification_attr::APP_IDENTIFIER,
    notification_attr::TITLE,
    notification_attr::SUBTITLE,
    notification_attr::MESSAGE,
    notification_attr::DATE,
    notification_attr::POSITIVE_ACTION_LABEL,
    notification_attr::NEGATIVE_ACTION_LABEL,
];

pub struct Characteristics {
    pub notification_source: Characteristic,
    pub control_point: Characteristic,
    pub data_source: Characteristic,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub show_preexisting: bool,
    pub ignored_apps: Arc<[String]>,
    pub dismiss_sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Notification(u32),
    App(String),
    Action(u32, ActionId),
}

impl Request {
    fn uid(&self) -> Option<u32> {
        match self {
            Request::Notification(uid) | Request::Action(uid, _) => Some(*uid),
            Request::App(_) => None,
        }
    }
}

struct InFlight {
    request: Request,
    assembler: Assembler,
    deadline: Instant,
}

struct Fetched {
    uid: u32,
    app_id: String,
    title: String,
    subtitle: String,
    message: String,
    positive_label: String,
    negative_label: String,
}

struct State<'a> {
    options: Options,
    desktop: &'a mut DeviceNotifications,
    queue: VecDeque<Request>,
    in_flight: Option<InFlight>,
    events: HashMap<u32, SourceEvent>,
    app_names: HashMap<String, String>,
    awaiting_app: HashMap<String, Vec<Fetched>>,
    cancelled: HashSet<u32>,
}

pub async fn run(
    chars: &Characteristics,
    desktop: &mut DeviceNotifications,
    options: Options,
    subscribed: impl FnOnce(),
) -> Result<()> {
    let signals = desktop.signals().await?;
    let data_source = subscribe(&chars.data_source, "Data Source").await;
    let notification_source = subscribe(&chars.notification_source, "Notification Source").await;
    info!("ANCS subscribed on {}", desktop.origin());
    subscribed();
    tokio::pin!(data_source, notification_source, signals);

    let mut state = State {
        options,
        desktop,
        queue: VecDeque::new(),
        in_flight: None,
        events: HashMap::new(),
        app_names: HashMap::new(),
        awaiting_app: HashMap::new(),
        cancelled: HashSet::new(),
    };

    loop {
        if state.in_flight.is_none() {
            state.start_next(&chars.control_point).await;
        }
        let deadline = state.in_flight.as_ref().map(|f| f.deadline);

        tokio::select! {
            value = notification_source.next() => match value {
                Some(value) => state.on_source(&value).await,
                None => bail!("Notification Source stream ended"),
            },
            value = data_source.next() => match value {
                Some(value) => state.on_data(&value).await,
                None => bail!("Data Source stream ended"),
            },
            Some(signal) = signals.next() => {
                if let Some((uid, action)) = state.desktop.resolve(signal).await {
                    state.on_user(uid, action);
                }
            }
            _ = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                state.on_timeout().await;
            }
        }
    }
}

async fn subscribe(
    characteristic: &Characteristic,
    name: &str,
) -> impl Stream<Item = Vec<u8>> + use<> {
    let mut warned = false;
    loop {
        match characteristic.notify().await {
            Ok(stream) => return stream,
            Err(err) if !warned => {
                warn!("cannot subscribe to {name} yet ({err}); retrying while connected");
                warned = true;
            }
            Err(err) => debug!("subscribe to {name} failed: {err}"),
        }
        sleep(SUBSCRIBE_RETRY).await;
    }
}

fn write_request() -> CharacteristicWriteRequest {
    CharacteristicWriteRequest {
        op_type: WriteOp::Request,
        ..Default::default()
    }
}

fn att_error_code(err: &bluer::Error) -> Option<u8> {
    let hex = err.message.split("0x").nth(1)?;
    u8::from_str_radix(hex.get(..2)?, 16).ok()
}

impl State<'_> {
    fn enqueue(&mut self, request: Request) {
        if self.queue.contains(&request)
            || self
                .in_flight
                .as_ref()
                .is_some_and(|f| f.request == request)
        {
            return;
        }
        if self.queue.len() >= MAX_QUEUED_REQUESTS {
            warn!("request queue full, dropping {request:?}");
            return;
        }
        self.queue.push_back(request);
    }

    async fn on_source(&mut self, value: &[u8]) {
        let Some(event) = SourceEvent::parse(value) else {
            warn!("malformed Notification Source packet: {value:02x?}");
            return;
        };
        debug!("source event {event:?}");

        match event.event {
            EventId::Added | EventId::Modified => {
                let shown = self.desktop.is_shown(event.uid);
                if event.has(event_flags::PRE_EXISTING) && !self.options.show_preexisting && !shown
                {
                    return;
                }
                self.cancelled.remove(&event.uid);
                self.events.insert(event.uid, event);
                self.enqueue(Request::Notification(event.uid));
            }
            EventId::Removed => {
                self.events.remove(&event.uid);
                self.queue.retain(|r| r.uid() != Some(event.uid));
                for waiting in self.awaiting_app.values_mut() {
                    waiting.retain(|f| f.uid != event.uid);
                }
                if self
                    .in_flight
                    .as_ref()
                    .is_some_and(|f| f.request == Request::Notification(event.uid))
                {
                    self.cancelled.insert(event.uid);
                }
                self.desktop.close(event.uid).await;
            }
        }
    }

    fn on_user(&mut self, uid: u32, action: UserAction) {
        let Some(event) = self.events.get(&uid) else {
            return;
        };
        let action = match action {
            UserAction::Positive => ActionId::Positive,
            UserAction::Negative => ActionId::Negative,
            UserAction::Dismissed
                if self.options.dismiss_sync && event.category != CategoryId::IncomingCall =>
            {
                ActionId::Negative
            }
            UserAction::Dismissed => return,
        };
        let offered = match action {
            ActionId::Positive => event.has(event_flags::POSITIVE_ACTION),
            ActionId::Negative => event.has(event_flags::NEGATIVE_ACTION),
        };
        if offered {
            debug!("notification {uid}: {action:?} action from the desktop");
            self.enqueue(Request::Action(uid, action));
        }
    }

    async fn start_next(&mut self, control_point: &Characteristic) {
        while let Some(request) = self.queue.pop_front() {
            let encoded = match &request {
                Request::Notification(uid) => {
                    protocol::get_notification_attributes(*uid, REQUESTED_ATTRIBUTES)
                }
                Request::App(app_id) => protocol::get_app_attributes(app_id),
                Request::Action(uid, action) => {
                    let payload = protocol::perform_notification_action(*uid, *action);
                    debug!("{request:?}: writing Control Point {payload:02x?}");
                    if let Err(err) = control_point.write_ext(&payload, &write_request()).await {
                        match att_error_code(&err) {
                            Some(att_error::INVALID_PARAMETER) => debug!("{request:?}: stale UID"),
                            _ => warn!("{request:?} failed: {err}"),
                        }
                    }
                    continue;
                }
            };
            debug!(
                "{request:?}: writing Control Point {:02x?}",
                encoded.payload
            );
            match control_point
                .write_ext(&encoded.payload, &write_request())
                .await
            {
                Ok(()) => {
                    self.in_flight = Some(InFlight {
                        request,
                        assembler: Assembler::new(encoded.expected),
                        deadline: Instant::now() + REQUEST_TIMEOUT,
                    });
                    return;
                }
                Err(err) => {
                    match att_error_code(&err) {
                        Some(att_error::INVALID_PARAMETER) => debug!("{request:?}: stale UID"),
                        Some(
                            att_error::UNKNOWN_COMMAND
                            | att_error::INVALID_COMMAND
                            | att_error::ACTION_FAILED,
                        ) => {
                            warn!("{request:?} rejected by the iPhone: {err}")
                        }
                        _ => warn!("{request:?} failed: {err}"),
                    }
                    self.fail(request).await;
                }
            }
        }
    }

    async fn on_data(&mut self, value: &[u8]) {
        debug!(
            "Data Source fragment ({} bytes): {:02x?}",
            value.len(),
            &value[..value.len().min(16)]
        );
        let Some(in_flight) = self.in_flight.as_mut() else {
            debug!("Data Source fragment with no request in flight, dropped");
            return;
        };
        match in_flight.assembler.feed(value) {
            Progress::Incomplete => {}
            Progress::Invalid(reason) => {
                let in_flight = self.in_flight.take().unwrap();
                warn!("{:?}: invalid response: {reason}", in_flight.request);
                self.fail(in_flight.request).await;
            }
            Progress::Complete(response) => {
                self.in_flight = None;
                self.on_response(response).await;
            }
        }
    }

    async fn on_timeout(&mut self) {
        if let Some(in_flight) = self.in_flight.take() {
            warn!(
                "{:?}: no response within {REQUEST_TIMEOUT:?}",
                in_flight.request
            );
            self.fail(in_flight.request).await;
        }
    }

    async fn fail(&mut self, request: Request) {
        if let Request::App(app_id) = request {
            self.app_names.insert(app_id.clone(), app_id.clone());
            for fetched in self.awaiting_app.remove(&app_id).unwrap_or_default() {
                self.show(fetched).await;
            }
        }
    }

    async fn on_response(&mut self, response: Response) {
        match response.command {
            CommandId::GetNotificationAttributes => {
                if self.cancelled.remove(&response.uid) || !self.events.contains_key(&response.uid)
                {
                    return;
                }
                let date = response.attribute(notification_attr::DATE);
                debug!("notification {} dated {date}", response.uid);
                if !self.options.show_preexisting
                    && !self.desktop.is_shown(response.uid)
                    && let Some(age) = age(date, chrono::Local::now().naive_local())
                    && age > MAX_AGE
                {
                    debug!(
                        "skipping notification {} posted {} min ago",
                        response.uid,
                        age.num_minutes()
                    );
                    return;
                }
                let fetched = Fetched {
                    uid: response.uid,
                    app_id: response
                        .attribute(notification_attr::APP_IDENTIFIER)
                        .to_owned(),
                    title: response.attribute(notification_attr::TITLE).to_owned(),
                    subtitle: response.attribute(notification_attr::SUBTITLE).to_owned(),
                    message: response.attribute(notification_attr::MESSAGE).to_owned(),
                    positive_label: response
                        .attribute(notification_attr::POSITIVE_ACTION_LABEL)
                        .to_owned(),
                    negative_label: response
                        .attribute(notification_attr::NEGATIVE_ACTION_LABEL)
                        .to_owned(),
                };
                if self.options.ignored_apps.contains(&fetched.app_id) {
                    debug!("ignoring notification {} from {}", fetched.uid, fetched.app_id);
                    return;
                }
                if self.app_names.contains_key(&fetched.app_id) {
                    self.show(fetched).await;
                } else {
                    let app_id = fetched.app_id.clone();
                    self.awaiting_app
                        .entry(app_id.clone())
                        .or_default()
                        .push(fetched);
                    self.enqueue(Request::App(app_id));
                }
            }
            CommandId::GetAppAttributes => {
                let name = response.attribute(protocol::app_attr::DISPLAY_NAME);
                let name = if name.is_empty() {
                    response.app_id.clone()
                } else {
                    name.to_owned()
                };
                self.app_names.insert(response.app_id.clone(), name);
                for fetched in self
                    .awaiting_app
                    .remove(&response.app_id)
                    .unwrap_or_default()
                {
                    self.show(fetched).await;
                }
            }
            CommandId::PerformNotificationAction => {}
        }
    }

    async fn show(&mut self, fetched: Fetched) {
        let Some(event) = self.events.get(&fetched.uid).copied() else {
            return;
        };
        let app_name = self
            .app_names
            .get(&fetched.app_id)
            .cloned()
            .unwrap_or_else(|| fetched.app_id.clone());
        let is_call = event.category == CategoryId::IncomingCall;
        let content = Content {
            app_id: fetched.app_id,
            app_name,
            title: fetched.title,
            subtitle: fetched.subtitle,
            message: fetched.message,
            category: event.category,
            silent: event.has(event_flags::SILENT),
            important: event.has(event_flags::IMPORTANT),
            positive_action: event
                .has(event_flags::POSITIVE_ACTION)
                .then(|| label_or(fetched.positive_label, "Accept")),
            negative_action: (event.has(event_flags::NEGATIVE_ACTION)
                && (!self.options.dismiss_sync || is_call))
                .then(|| label_or(fetched.negative_label, if is_call { "Decline" } else { "Dismiss" })),
        };
        self.desktop.show(fetched.uid, &content).await;
    }
}

fn label_or(label: String, fallback: &str) -> String {
    if label.is_empty() { fallback.to_owned() } else { label }
}

/// ANCS dates are the iPhone's local time, with no zone,
/// so this assumes the iPhone and the desktop share a time zone.
fn age(date: &str, now: chrono::NaiveDateTime) -> Option<chrono::TimeDelta> {
    let posted = chrono::NaiveDateTime::parse_from_str(date, "%Y%m%dT%H%M%S").ok()?;
    Some(now - posted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_ancs_dates() {
        let now = chrono::NaiveDateTime::parse_from_str("20261012T214900", "%Y%m%dT%H%M%S").unwrap();
        assert_eq!(age("20261012T214500", now), Some(chrono::TimeDelta::minutes(4)));
        assert!(age("20261012T215000", now).unwrap() < MAX_AGE);
        assert_eq!(age("", now), None);
        assert_eq!(age("garbage", now), None);
    }

    #[test]
    fn parses_att_error_from_bluez_message() {
        let err = bluer::Error {
            kind: bluer::ErrorKind::Failed,
            message: "Operation failed with ATT error: 0xa2".into(),
        };
        assert_eq!(att_error_code(&err), Some(att_error::INVALID_PARAMETER));
        let err = bluer::Error {
            kind: bluer::ErrorKind::Failed,
            message: "Not connected".into(),
        };
        assert_eq!(att_error_code(&err), None);
    }
}
