use capsule_sdk::{AuthenticatedClient, Client, types::AuthModelsRequestsLoginRequest};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_url =
        env::var("CAPSULE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let client = Client::new(&base_url);

    let email = String::from("johndoe@email.com");
    let password = String::from("password");

    println!("Logging in with: {}; {}", email, password);

    let login_req = AuthModelsRequestsLoginRequest { email, password };
    let res = match client.login_user(&login_req).await {
        Ok(res) => {
            println!("Login successful!");
            res
        }
        Err(e) => {
            eprintln!("Failed to login: {:?}", e);
            std::process::exit(1);
        }
    };
    println!("Access Token: {}", res.access_token);

    // Verify profile
    println!("Fetching profile...");
    let authenticated_client = AuthenticatedClient::new(&base_url, &res.access_token);
    let profile = authenticated_client.get_user_profile().await?;
    println!("Profile: {:#?}", profile);

    Ok(())
}
