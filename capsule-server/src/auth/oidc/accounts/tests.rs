use super::*;

fn identity(subject: &str, email: Option<&str>) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: "https://idp.example.test".to_owned(),
        subject: subject.to_owned(),
        email: email.map(str::to_owned),
        email_verified: true,
    }
}

fn user(tag: &str) -> UserId {
    UserId::new(format!("user-{tag}"))
}

// What each adapter does with an identity is asserted once, against both adapters, by the
// shared suite in `crate::auth::conformance`. What is left here is the rule itself and the
// null object.

#[test]
fn only_a_verified_address_is_reserved() {
    assert_eq!(
        reserved_address(&identity("sub-1", Some("a@example.test"))),
        Some("a@example.test")
    );
    let mut unverified = identity("sub-1", Some("a@example.test"));
    unverified.email_verified = false;
    assert_eq!(reserved_address(&unverified), None);
    assert_eq!(reserved_address(&identity("sub-1", None)), None);
}

#[tokio::test]
async fn the_disabled_directory_refuses() {
    assert!(
        Disabled
            .resolve_or_create(&identity("sub-1", None), &user("1"), Timestamp::UNIX_EPOCH)
            .await
            .is_err()
    );
}
