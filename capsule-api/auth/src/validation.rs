use crate::models::errors::BadRegisterUserRequestError;
use crate::models::requests::RegisterRequest;
use secrecy::ExposeSecret;
use service::user as UserService;

pub struct RegistrationValidator;

impl RegistrationValidator {
    pub fn validate(request: &RegisterRequest) -> Result<(), BadRegisterUserRequestError> {
        Self::validate_username(&request.username)?;
        Self::validate_email(&request.email)?;
        Self::validate_password(request.password.expose_secret())?;
        Ok(())
    }

    fn validate_username(username: &str) -> Result<(), BadRegisterUserRequestError> {
        if !UserService::is_valid_username(username) {
            return Err(BadRegisterUserRequestError::Username);
        }
        Ok(())
    }

    fn validate_email(email: &str) -> Result<(), BadRegisterUserRequestError> {
        if !UserService::is_valid_email(email) {
            return Err(BadRegisterUserRequestError::Email);
        }
        Ok(())
    }

    fn validate_password(password: &str) -> Result<(), BadRegisterUserRequestError> {
        if !UserService::is_valid_password(password) {
            return Err(BadRegisterUserRequestError::Password);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_request() {
        let request = RegisterRequest {
            username: "valid_user".to_string(),
            email: "test@example.com".to_string(),
            password: "Password123!".into(),
            name: "Test User".to_string(),
        };
        assert!(RegistrationValidator::validate(&request).is_ok());
    }
}
