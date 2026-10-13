use uuid::{Uuid, uuid};

pub const SERVICE: Uuid = uuid!("7905f431-b5ce-4e99-a40f-4b1e122d00d0");
pub const NOTIFICATION_SOURCE: Uuid = uuid!("9fbf120d-6301-42d9-8c58-25e699a21dbd");
pub const CONTROL_POINT: Uuid = uuid!("69d1d8f3-45e1-49a8-9821-9bbdfdaad9d9");
pub const DATA_SOURCE: Uuid = uuid!("22eac6e9-24d6-4bb5-be44-b36ace7c7bfb");

pub mod att_error {
    pub const UNKNOWN_COMMAND: u8 = 0xa0;
    pub const INVALID_COMMAND: u8 = 0xa1;
    pub const INVALID_PARAMETER: u8 = 0xa2;
    pub const ACTION_FAILED: u8 = 0xa3;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventId {
    Added,
    Modified,
    Removed,
}

pub mod event_flags {
    pub const SILENT: u8 = 1 << 0;
    pub const IMPORTANT: u8 = 1 << 1;
    pub const PRE_EXISTING: u8 = 1 << 2;
    pub const POSITIVE_ACTION: u8 = 1 << 3;
    pub const NEGATIVE_ACTION: u8 = 1 << 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryId {
    Other,
    IncomingCall,
    MissedCall,
    Voicemail,
    Social,
    Schedule,
    Email,
    News,
    HealthAndFitness,
    BusinessAndFinance,
    Location,
    Entertainment,
}

impl CategoryId {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::IncomingCall,
            2 => Self::MissedCall,
            3 => Self::Voicemail,
            4 => Self::Social,
            5 => Self::Schedule,
            6 => Self::Email,
            7 => Self::News,
            8 => Self::HealthAndFitness,
            9 => Self::BusinessAndFinance,
            10 => Self::Location,
            11 => Self::Entertainment,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandId {
    GetNotificationAttributes = 0,
    GetAppAttributes = 1,
    PerformNotificationAction = 2,
}

pub mod notification_attr {
    pub const APP_IDENTIFIER: u8 = 0;
    pub const TITLE: u8 = 1;
    pub const SUBTITLE: u8 = 2;
    pub const MESSAGE: u8 = 3;
    pub const DATE: u8 = 5;
    pub const POSITIVE_ACTION_LABEL: u8 = 6;
    pub const NEGATIVE_ACTION_LABEL: u8 = 7;

    pub fn takes_length(id: u8) -> bool {
        matches!(id, TITLE | SUBTITLE | MESSAGE)
    }
}

pub mod app_attr {
    pub const DISPLAY_NAME: u8 = 0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionId {
    Positive = 0,
    Negative = 1,
}

pub const MAX_TITLE_LENGTH: u16 = 128;
pub const MAX_SUBTITLE_LENGTH: u16 = 128;
pub const MAX_MESSAGE_LENGTH: u16 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceEvent {
    pub event: EventId,
    pub flags: u8,
    pub category: CategoryId,
    pub category_count: u8,
    pub uid: u32,
}

impl SourceEvent {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != 8 {
            return None;
        }
        let event = match data[0] {
            0 => EventId::Added,
            1 => EventId::Modified,
            2 => EventId::Removed,
            _ => return None,
        };
        Some(Self {
            event,
            flags: data[1],
            category: CategoryId::from_u8(data[2]),
            category_count: data[3],
            uid: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub command: CommandId,
    pub attributes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub payload: Vec<u8>,
    pub expected: Expected,
}

fn max_length(id: u8) -> u16 {
    match id {
        notification_attr::TITLE => MAX_TITLE_LENGTH,
        notification_attr::SUBTITLE => MAX_SUBTITLE_LENGTH,
        _ => MAX_MESSAGE_LENGTH,
    }
}

pub fn get_notification_attributes(uid: u32, attributes: &[u8]) -> Encoded {
    let mut payload = vec![CommandId::GetNotificationAttributes as u8];
    payload.extend_from_slice(&uid.to_le_bytes());
    for &id in attributes {
        payload.push(id);
        if notification_attr::takes_length(id) {
            payload.extend_from_slice(&max_length(id).to_le_bytes());
        }
    }
    Encoded {
        payload,
        expected: Expected {
            command: CommandId::GetNotificationAttributes,
            attributes: attributes.to_vec(),
        },
    }
}

pub fn get_app_attributes(app_id: &str) -> Encoded {
    let mut payload = vec![CommandId::GetAppAttributes as u8];
    payload.extend_from_slice(app_id.as_bytes());
    payload.push(0);
    payload.push(app_attr::DISPLAY_NAME);
    Encoded {
        payload,
        expected: Expected {
            command: CommandId::GetAppAttributes,
            attributes: vec![app_attr::DISPLAY_NAME],
        },
    }
}

pub fn perform_notification_action(uid: u32, action: ActionId) -> Vec<u8> {
    let mut payload = vec![CommandId::PerformNotificationAction as u8];
    payload.extend_from_slice(&uid.to_le_bytes());
    payload.push(action as u8);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_source_event() {
        let event = SourceEvent::parse(&[0, 0x02, 1, 1, 0x40, 0x30, 0x20, 0x10]).unwrap();
        assert_eq!(event.event, EventId::Added);
        assert_eq!(event.category, CategoryId::IncomingCall);
        assert_eq!(event.category_count, 1);
        assert_eq!(event.uid, 0x10203040);
        assert!(event.has(event_flags::IMPORTANT));
        assert!(!event.has(event_flags::PRE_EXISTING));
    }

    #[test]
    fn rejects_bad_source_event() {
        assert!(SourceEvent::parse(&[0; 7]).is_none());
        assert!(SourceEvent::parse(&[3, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert_eq!(
            SourceEvent::parse(&[2, 0, 200, 0, 1, 0, 0, 0])
                .unwrap()
                .category,
            CategoryId::Other
        );
    }

    #[test]
    fn encodes_notification_request_with_lengths() {
        use notification_attr::*;
        let req = get_notification_attributes(7, &[APP_IDENTIFIER, TITLE, MESSAGE, DATE]);
        assert_eq!(
            req.payload,
            vec![
                0,
                7,
                0,
                0,
                0,
                APP_IDENTIFIER,
                TITLE,
                128,
                0,
                MESSAGE,
                0,
                2,
                DATE
            ]
        );
        assert_eq!(
            req.expected.attributes,
            vec![APP_IDENTIFIER, TITLE, MESSAGE, DATE]
        );
    }

    #[test]
    fn encodes_app_request() {
        let req = get_app_attributes("com.a");
        assert_eq!(req.payload, b"\x01com.a\x00\x00".to_vec());
    }

    #[test]
    fn encodes_action() {
        assert_eq!(
            perform_notification_action(1, ActionId::Negative),
            vec![2, 1, 0, 0, 0, 1]
        );
    }
}
