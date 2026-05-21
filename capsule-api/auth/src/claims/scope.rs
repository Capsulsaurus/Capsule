use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter, Hash)]
pub enum Scope {
    #[serde(rename = "token:access")]
    AccessToken,
    #[serde(rename = "token:refresh")]
    RefreshToken,
    #[serde(rename = "token:mfa")]
    MfaToken,
    #[serde(rename = "read:user")]
    ReadUser,
    #[serde(rename = "write:user")]
    WriteUser,
}

impl From<&Scope> for String {
    fn from(scope: &Scope) -> Self {
        match scope {
            Scope::AccessToken => "token:access".to_string(),
            Scope::RefreshToken => "token:refresh".to_string(),
            Scope::MfaToken => "token:mfa".to_string(),
            Scope::ReadUser => "read:user".to_string(),
            Scope::WriteUser => "write:user".to_string(),
        }
    }
}

impl From<Scope> for String {
    fn from(scope: Scope) -> Self {
        (&scope).into()
    }
}

impl FromStr for Scope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "token:access" => Ok(Scope::AccessToken),
            "token:refresh" => Ok(Scope::RefreshToken),
            "token:mfa" => Ok(Scope::MfaToken),
            "read:user" => Ok(Scope::ReadUser),
            "write:user" => Ok(Scope::WriteUser),
            _ => Err(format!("Invalid scope: {s}")),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", String::from(self))
    }
}

impl Scope {
    pub fn all_scopes() -> Vec<String> {
        use strum::IntoEnumIterator;
        Self::iter().map(String::from).collect()
    }
}

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn test_scope_from_str() {
        for scope in Scope::iter() {
            let scope_str: String = scope.into();
            assert_eq!(scope, Scope::from_str(&scope_str).unwrap());
        }
    }

    #[test]
    fn test_scope_from_str_invalid() {
        let invalid_scope = "invalid:scope";
        assert!(Scope::from_str(invalid_scope).is_err());
    }

    #[test]
    fn test_serialize_deserialize_json() {
        for scope in Scope::iter() {
            let scope_str: String = scope.into();
            let serialized = serde_json::to_string(&scope).unwrap();
            assert_eq!(format!("\"{}\"", scope_str), serialized);

            let deserialized: Scope = serde_json::from_str(&serialized).unwrap();
            assert_eq!(scope, deserialized);
        }
    }

    #[test]
    fn test_serialize_deserialize_json_invalid() {
        let invalid_scope = "invalid:scope";
        assert!(serde_json::from_str::<Scope>(invalid_scope).is_err());
    }
}
