use salvo::prelude::*;
use thiserror::Error;

// TODO: Nothing is using this?

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("Asset not found")]
    NotFound,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden")]
    Forbidden,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<sea_orm::DbErr> for MediaError {
    fn from(err: sea_orm::DbErr) -> Self {
        MediaError::Internal(err.to_string())
    }
}

#[async_trait]
impl Writer for MediaError {
    async fn write(mut self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        let code = match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        res.status_code(code);
        res.render(Text::Plain(self.to_string()));
    }
}
