use super::{ServerInfoReport, server_info_lines};

fn lines(oidc: bool, password_disabled: bool) -> Vec<String> {
    server_info_lines(&ServerInfoReport {
        server_id: "https://capsule.example".to_owned(),
        oidc,
        password_disabled,
    })
}

/// Every combination of the two flags prints the server, then the identity provider row,
/// then the password row.
#[test]
fn each_flag_combination_prints_its_own_rows() {
    let cases = [
        (false, false, "not configured", "accepted"),
        (false, true, "not configured", "refused"),
        (
            true,
            false,
            "available (capsule auth login --oidc)",
            "accepted",
        ),
        (
            true,
            true,
            "available (capsule auth login --oidc)",
            "refused",
        ),
    ];
    for (oidc, password_disabled, provider, password) in cases {
        assert_eq!(
            lines(oidc, password_disabled),
            vec![
                "Server: https://capsule.example".to_owned(),
                format!("Identity provider: {provider}"),
                format!("Password sign-in: {password}"),
            ],
            "oidc={oidc} password_disabled={password_disabled}"
        );
    }
}
