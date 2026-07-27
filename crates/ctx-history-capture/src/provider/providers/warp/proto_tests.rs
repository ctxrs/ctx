use super::*;

fn field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = varint(u64::from(field) << 3 | 2);
    encoded.extend(varint(payload.len() as u64));
    encoded.extend_from_slice(payload);
    encoded
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

#[test]
fn task_and_user_message_fields_decode_without_projection() {
    let mut user_query = field(1, b"hello");
    user_query.extend(field(99, b"ignored"));
    let mut message = field(1, b"message-1");
    message.extend(field(2, &user_query));
    let mut dependency = field(1, b"parent-task");
    dependency.extend(field(2, b"ignored"));
    let mut task = field(1, b"task-1");
    task.extend(field(2, b"description"));
    task.extend(field(3, &dependency));
    task.extend(field(5, &message));
    task.extend(field(6, b"summary"));

    let decoded = warp_decode_task(&task).unwrap();
    assert_eq!(decoded.id, "task-1");
    assert_eq!(decoded.description, "description");
    assert_eq!(decoded.parent_task_id.as_deref(), Some("parent-task"));
    assert_eq!(decoded.summary, "summary");
    assert_eq!(decoded.messages.len(), 1);
    assert_eq!(decoded.messages[0].kind, "user_query");
    assert_eq!(decoded.messages[0].role, Some(EventRole::User));
    assert_eq!(decoded.messages[0].text, "hello");
}

#[test]
fn wire_failures_remain_bounded_and_fail_closed() {
    let cases: &[(&[u8], &str)] = &[
        (&[0x0a, 0x02, b'a'], "truncated length-delimited field"),
        (&[0x0a, 0x01, 0xff], "invalid UTF-8"),
        (&[0x0b], "unsupported Warp protobuf wire type 3"),
        (&[0x80; 10], "oversized varint"),
    ];
    for (encoded, expected) in cases {
        let error = warp_decode_task(encoded).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error}"
        );
    }
}
