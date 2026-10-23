use kynos::security::SecurityScheme as _;

use super::ReadBearer;
use crate::auth::AccessToken;

#[test]
fn the_two_schemes_describe_one_component() {
    // What keeps `components.securitySchemes` at one `bearer` entry and every operation's
    // `security` unchanged: Kynos accepts a second registration under a name exactly when
    // its description is byte-identical to the first.
    assert_eq!(ReadBearer::NAME, AccessToken::NAME);
    assert_eq!(ReadBearer::describe(), AccessToken::describe());
    assert_eq!(ReadBearer::challenge(), AccessToken::challenge());
}
