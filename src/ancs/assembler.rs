use super::protocol::{CommandId, Expected};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub command: CommandId,
    pub uid: u32,
    pub app_id: String,
    pub attributes: Vec<(u8, String)>,
}

impl Response {
    pub fn attribute(&self, id: u8) -> &str {
        self.attributes
            .iter()
            .find(|(attr, _)| *attr == id)
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Incomplete,
    Complete(Response),
    Invalid(&'static str),
}

pub struct Assembler {
    expected: Expected,
    buffer: Vec<u8>,
}

impl Assembler {
    pub fn new(expected: Expected) -> Self {
        Self {
            expected,
            buffer: Vec::new(),
        }
    }

    pub fn feed(&mut self, fragment: &[u8]) -> Progress {
        self.buffer.extend_from_slice(fragment);
        parse(&self.buffer, &self.expected)
    }
}

fn parse(buf: &[u8], expected: &Expected) -> Progress {
    let Some(&command) = buf.first() else {
        return Progress::Incomplete;
    };
    if command != expected.command as u8 {
        return Progress::Invalid("response is for a different command");
    }
    let mut pos = 1;

    let mut uid = 0;
    let mut app_id = String::new();
    match expected.command {
        CommandId::GetNotificationAttributes => {
            let Some(bytes) = buf.get(pos..pos + 4) else {
                return Progress::Incomplete;
            };
            uid = u32::from_le_bytes(bytes.try_into().unwrap());
            pos += 4;
        }
        CommandId::GetAppAttributes => {
            let Some(nul) = buf[pos..].iter().position(|&b| b == 0) else {
                return Progress::Incomplete;
            };
            app_id = String::from_utf8_lossy(&buf[pos..pos + nul]).into_owned();
            pos += nul + 1;
        }
        CommandId::PerformNotificationAction => return Progress::Invalid("action has no response"),
    }

    let mut attributes = Vec::with_capacity(expected.attributes.len());
    for &want in &expected.attributes {
        let Some(header) = buf.get(pos..pos + 3) else {
            return Progress::Incomplete;
        };
        if header[0] != want {
            return Progress::Invalid("attributes out of the requested order");
        }
        let len = u16::from_le_bytes([header[1], header[2]]) as usize;
        pos += 3;
        let Some(value) = buf.get(pos..pos + len) else {
            return Progress::Incomplete;
        };
        attributes.push((want, String::from_utf8_lossy(value).into_owned()));
        pos += len;
    }

    if pos != buf.len() {
        return Progress::Invalid("trailing bytes after the last attribute");
    }

    Progress::Complete(Response {
        command: expected.command,
        uid,
        app_id,
        attributes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ancs::protocol::{
        get_app_attributes, get_notification_attributes, notification_attr::*,
    };

    fn attr(id: u8, value: &str) -> Vec<u8> {
        let mut out = vec![id];
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(value.as_bytes());
        out
    }

    fn notification_response() -> (Expected, Vec<u8>) {
        let req =
            get_notification_attributes(0x01020304, &[APP_IDENTIFIER, TITLE, SUBTITLE, MESSAGE]);
        let mut bytes = vec![0, 0x04, 0x03, 0x02, 0x01];
        bytes.extend(attr(APP_IDENTIFIER, "com.apple.MobileSMS"));
        bytes.extend(attr(TITLE, "Alice"));
        bytes.extend(attr(SUBTITLE, ""));
        bytes.extend(attr(MESSAGE, "héllo"));
        (req.expected, bytes)
    }

    fn assert_notification(response: Response) {
        assert_eq!(response.uid, 0x01020304);
        assert_eq!(response.attribute(APP_IDENTIFIER), "com.apple.MobileSMS");
        assert_eq!(response.attribute(TITLE), "Alice");
        assert_eq!(response.attribute(SUBTITLE), "");
        assert_eq!(response.attribute(MESSAGE), "héllo");
    }

    #[test]
    fn single_packet() {
        let (expected, bytes) = notification_response();
        match Assembler::new(expected).feed(&bytes) {
            Progress::Complete(r) => assert_notification(r),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_split_point() {
        let (expected, bytes) = notification_response();
        for split in 1..bytes.len() {
            let mut asm = Assembler::new(expected.clone());
            assert_eq!(
                asm.feed(&bytes[..split]),
                Progress::Incomplete,
                "split at {split}"
            );
            match asm.feed(&bytes[split..]) {
                Progress::Complete(r) => assert_notification(r),
                other => panic!("split at {split}: {other:?}"),
            }
        }
    }

    #[test]
    fn byte_by_byte() {
        let (expected, bytes) = notification_response();
        let mut asm = Assembler::new(expected);
        let (last, head) = bytes.split_last().unwrap();
        for b in head {
            assert_eq!(asm.feed(&[*b]), Progress::Incomplete);
        }
        assert!(matches!(asm.feed(&[*last]), Progress::Complete(_)));
    }

    #[test]
    fn app_attributes() {
        let req = get_app_attributes("com.example");
        let mut bytes = b"\x01com.example\x00".to_vec();
        bytes.extend(attr(0, "Example"));
        let mut asm = Assembler::new(req.expected);
        assert_eq!(asm.feed(&bytes[..5]), Progress::Incomplete);
        match asm.feed(&bytes[5..]) {
            Progress::Complete(r) => {
                assert_eq!(r.app_id, "com.example");
                assert_eq!(r.attribute(0), "Example");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_command_and_order() {
        let (expected, mut bytes) = notification_response();
        let mut asm = Assembler::new(expected.clone());
        assert!(matches!(asm.feed(&[1]), Progress::Invalid(_)));

        bytes[5] = TITLE;
        assert!(matches!(
            Assembler::new(expected).feed(&bytes),
            Progress::Invalid(_)
        ));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let (expected, mut bytes) = notification_response();
        bytes.push(9);
        assert!(matches!(
            Assembler::new(expected).feed(&bytes),
            Progress::Invalid(_)
        ));
    }
}
