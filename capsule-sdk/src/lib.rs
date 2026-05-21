progenitor::generate_api!("openapi.json");

pub mod upload;

/// Authenticated OpenAPI client
pub struct AuthenticatedClient {
    /// Base URL of the API
    base_url: String,
    /// Current access token
    access_token: String,
    /// OpenAPI client
    client: Client,
}

impl AuthenticatedClient {
    pub fn new(base_url: &str, access_token: &str) -> AuthenticatedClient {
        AuthenticatedClient {
            base_url: base_url.to_string(),
            access_token: access_token.to_string(),
            client: Self::get_authenticated_client(base_url, access_token),
        }
    }

    fn get_authenticated_client(base_url: &str, access_token: &str) -> Client {
        let authorization_header = format!("Bearer {}", access_token);

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            authorization_header.parse().unwrap(),
        );

        let client_with_custom_defaults = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()
            .unwrap();

        Client::new_with_client(base_url, client_with_custom_defaults)
    }

    /// Returns same instance with new base URL
    pub fn with_base_url(&mut self, base_url: &str) -> &mut Self {
        self.base_url = base_url.to_string();
        self.client = Self::get_authenticated_client(base_url, &self.access_token);
        self
    }

    /// Returns same instance with new access token
    pub fn with_access_token(&mut self, access_token: &str) -> &mut Self {
        self.access_token = access_token.to_string();
        self.client = Self::get_authenticated_client(&self.base_url, access_token);
        self
    }
}

impl std::ops::Deref for AuthenticatedClient {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
