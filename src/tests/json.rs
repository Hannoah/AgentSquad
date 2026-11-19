use crate::json::{self, JsonValue};

#[test]
fn json_roundtrip_preserves_order_and_escapes() {
    let raw = "\u{feff}{\n  \"z_first\": 42,\n  \"a_second\": true,\n  \"text\": \"line1\\n\\t\\\"quote\\\"\\\\slash\\uD83D\\uDE80\",\n  \"nested\": [null, false, -3.14e+2]\n}\n";
    let parsed = json::parse(raw).expect("valid JSON");
    let entries = parsed.as_object().expect("root object");
    assert_eq!(entries[0].0, "z_first");
    assert_eq!(entries[1].0, "a_second");
    assert_eq!(
        parsed.get("text"),
        Some(&JsonValue::String("line1\n\t\"quote\"\\slash🚀".into()))
    );
    let pretty = parsed.to_pretty();
    let reparsed = json::parse(&pretty).expect("reparse pretty");
    assert_eq!(parsed, reparsed);
}

#[test]
fn json_rejects_malformed_and_trailing_input() {
    assert!(json::parse("").is_err());
    assert!(json::parse("{bad: 1}").is_err());
    assert!(json::parse("{\"a\": 1,}").is_err());
    assert!(json::parse("{\"a\": 1} extra").is_err());
    assert!(json::parse("\"unterminated").is_err());
}
