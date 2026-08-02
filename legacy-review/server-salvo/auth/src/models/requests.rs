use salvo::oapi::ToSchema;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[salvo(schema(
    example = json!({
        "username": "johndoe",
        "name": "John Doe",
        "email": "johndoe@email.com",
        "password": "password"
    })
))]
pub struct RegisterRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub password: SecretString,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[salvo(schema(
    example = json!({
        "email": "johndoe@email.com",
        "password": "password"
    })
))]
pub struct LoginRequest {
    pub email: String,
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub password: SecretString,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct RefreshTokenRequest {
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub refresh_token: SecretString,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct UpdateProfileRequest {
    pub username: Option<String>,
    pub email: Option<String>,
    #[salvo(schema(value_type = Option<String>))]
    #[serde(serialize_with = "crate::models::serialize_secret_option")]
    pub current_password: Option<SecretString>,
    #[salvo(schema(value_type = Option<String>))]
    #[serde(serialize_with = "crate::models::serialize_secret_option")]
    pub new_password: Option<SecretString>,
}

// TOTP Request types

#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[salvo(schema(
    example = json!({
        "totp_code": "123456"
    })
))]
pub struct VerifyTotpEnrollmentRequest {
    pub totp_code: String,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[salvo(schema(
    example = json!({
        "mfa_token": "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9...",
        "totp_code": "123456"
    })
))]
pub struct VerifyTotpLoginRequest {
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub mfa_token: SecretString,
    pub totp_code: String,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[salvo(schema(
    example = json!({
        "totp_code": "123456"
    })
))]
pub struct DisableTotpRequest {
    pub totp_code: String,
}
