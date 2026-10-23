use jiff::Timestamp;

use super::ProfileResponse;
use crate::auth::ProfileRecord;
use crate::store::UserId;

fn record(email: Option<&str>) -> ProfileRecord {
    ProfileRecord {
        user_id: UserId::new("user-1"),
        email: email.map(str::to_owned),
        display_name: None,
        created_at: Timestamp::UNIX_EPOCH,
    }
}

fn keys(response: &ProfileResponse) -> Vec<String> {
    let value = serde_json::to_value(response).expect("a profile serializes");
    let mut keys: Vec<String> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort_unstable();
    keys
}

#[test]
fn an_addressless_profile_omits_the_email_key() {
    // Absent rather than `null`, the same convention `display_name` follows, so a client's
    // "has an address" test is a key test and a client generated before the field became
    // optional sees a missing key rather than a type it cannot decode.
    let response = ProfileResponse::from(record(None));
    assert_eq!(response.email, None);
    assert_eq!(keys(&response), ["created_at", "user_id"]);
}

#[test]
fn a_profile_with_an_address_serves_it() {
    let response = ProfileResponse::from(record(Some("ada@example.test")));
    assert_eq!(response.email.as_deref(), Some("ada@example.test"));
    assert_eq!(keys(&response), ["created_at", "email", "user_id"]);
}
