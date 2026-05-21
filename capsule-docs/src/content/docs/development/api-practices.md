---
title: API Development Practices
description: Best practices for development in the Capsule API, including error handling patterns, code organization, and architectural guidelines.
---

## Error Handling Architecture

The Capsule API uses an **onion architecture** for error handling, with distinct layers that each have specific responsibilities for how errors are created, propagated, and transformed.

### Architectural Layers

```plaintext
┌─────────────────────────────────────────────────────────────┐
│                      Network Layer (Salvo)                   │
│  Converts all unexpected errors to HTTP 500 responses        │
├─────────────────────────────────────────────────────────────┤
│                     Service Layer (Business Logic)           │
│  Uses Result<T, ServiceError> for expected client errors     │
├─────────────────────────────────────────────────────────────┤
│                    Data/External Layer                       │
│  Returns raw errors (DbErr, io::Error, etc.)                │
└─────────────────────────────────────────────────────────────┘
```

### Layer 1: Network Layer (Salvo Routes)

The network layer is the outermost layer and is responsible for:

1. **Authentication and Authorization** - Validate JWTs and permissions
2. **Request Parsing** - Extract and validate request data
3. **Response Serialization** - Convert service responses to HTTP responses
4. **Error Transformation** - Convert all errors to appropriate HTTP status codes

#### Error Handling Strategy

```rust
// Use typed response enums for expected outcomes
pub enum MyEndpointResponses {
    Success(SuccessData),
    BadRequest(String),       // 400 - Client error with message
    Unauthorized(String),     // 401 - Missing or invalid auth
    Forbidden,                // 403 - Insufficient permissions
    NotFound,                 // 404 - Resource not found
    Conflict(String),         // 409 - State conflict
    InternalServerError(InternalServerError),      // 500 - Internal server error
}
```

### Layer 2: Service Layer (Business Logic)

The service layer contains the core business logic and is responsible for:

1. **Business Rule Enforcement** - Validate business invariants
2. **Orchestration** - Coordinate multiple data operations
3. **Transaction Management** - Ensure data consistency
4. **Error Classification** - Distinguish between client errors and fatal errors

#### Error Handling Strategy

Service layer functions should use **typed error enums** for expected error states that have a "happy path" (where a client action can resolve the issue):

```rust
// Good: Domain-specific error with recoverable variants
#[derive(Debug, Error)]
pub enum FriendshipError {
    #[error("Database error: {0}")]
    DbError(#[from] DbErr),
    
    #[error("Not found")]
    NotFound,                    // Client can create the resource
    
    #[error("Not authorized")]
    NotAuthorized,               // Client can get proper auth
    
    #[error("Request is not pending")]
    NotPending,                  // Expected state, client can retry
}
```

For truly fatal/unexpected errors, use `eyre::Report` to bubble up with context:

```rust
use eyre::{Result, WrapErr, bail};

impl MyService {
    pub async fn complex_operation(&self) -> Result<MyData> {
        let data = self.data_layer
            .fetch_something()
            .await
            .wrap_err("Failed to fetch initial data")?;
        
        // Fatal: configuration is broken
        if data.is_corrupt() {
            bail!("Data corruption detected for id={}", data.id);
        }
        
        Ok(data)
    }
}
```

#### Guidelines

1. **Use `Err` variant only for unexpected/unrecoverable errors** - Errors that otherwise indicate a happy path should not use Err variants
2. **Use specific error enums for expected failures** - Errors that the client can handle (not found, unauthorized, validation failures)
3. **Wrap context on error propagation** - Use `.wrap_err()` or `.context()` to add meaningful context
4. **Never expose/log sensitive details** - Error messages should describe what happened, and not unnecessary/sensitive details

### Layer 3: Data/External Layer

The data layer interacts directly with databases, file systems, caches, and external services.

#### Error Handling Strategy

Return raw errors from external dependencies. Let the service layer decide how to handle them:

```rust
// Good: Return raw errors
impl Query {
    pub async fn find_user_by_id(
        db: &DbConn, 
        id: String
    ) -> Result<Option<user::Model>, DbErr> {
        User::find_by_id(id).one(db).await
    }
}

// Good: Raw file system errors
pub async fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    tokio::fs::read(path).await
}
```

Do **not** wrap errors at this layer:

```rust
// Bad: Wrapping at data layer loses type information
pub async fn find_user_by_id(db: &DbConn, id: String) -> Result<Option<user::Model>> {
    User::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| eyre::eyre!("Database error: {}", e))  // ❌ Don't do this
}
```

## Result Types and When to Use Them

### `Result<T, SpecificError>`

Use for service-layer operations where specific error variants matter:

```rust
// Friendship operations have specific, recoverable error states
pub async fn send_friend_request(
    db: &DatabaseConnection,
    user_id: &str,
    friend_id: &str,
) -> Result<SendRequestResult, FriendshipError> { ... }
```

### `Result<T, eyre::Report>`

Use for operations where any error is fatal/unexpected:

```rust
// Server initialization - any error is fatal
pub async fn create_router(conn: DatabaseConnection, env: &Environment) -> eyre::Result<Router> {
    // Errors here mean the server can't start
}
```

### `Option<T>`

Use when absence is a normal, expected outcome:

```rust
// User might not exist - that's not an error
pub async fn find_user_by_id(db: &DbConn, id: String) -> Result<Option<user::Model>, DbErr> { ... }
```

## API Response Patterns

### REST Endpoints (Salvo)

Define typed response enums that implement `Writer` and `EndpointOutRegister`:

```rust
pub enum GetAssetResponses {
    Success(AssetData),
    NotFound,
    Unauthorized(String),
    Forbidden,
}

#[async_trait]
impl Writer for GetAssetResponses {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(data) => {
                res.status_code(StatusCode::OK);
                res.render(Json(data));
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::Forbidden => {
                res.status_code(StatusCode::FORBIDDEN);
            }
        }
    }
}
```

### GraphQL (async-graphql)

Use `async_graphql::Result<T>` which wraps errors appropriately:

```rust
#[Object]
impl AssetQuery {
    async fn get_asset(&self, ctx: &Context<'_>, id: ID) -> Result<AssetMetadata> {
        let db = ctx.data::<DatabaseConnection>()?;
        let user = ctx.data::<UserContext>()?;
        
        let asset = AssetService::find_by_id(db, &id.to_string())
            .await?
            .ok_or_else(|| Error::new("Asset not found"))?;
        
        // Permission check
        if !user.can_access(&asset) {
            return Err(Error::new("Access denied"));
        }
        
        Ok(asset.into())
    }
}
```

## Logging Best Practices

### Structured Logging

Use `tracing` with structured fields:

```rust
use tracing::{info, warn, error, instrument};

#[instrument(skip(db), fields(user_id = %user_id))]
pub async fn create_asset(db: &DbConn, user_id: &str, input: CreateAssetInput) -> Result<Asset> {
    info!("Creating new asset");
    
    let asset = do_create(db, input).await.map_err(|e| {
        error!(?e, "Failed to create asset");
        e
    })?;
    
    info!(asset_id = %asset.id, "Asset created successfully");
    Ok(asset)
}
```

### Log Levels

- **ERROR**: Unexpected failures, requires investigation
- **WARN**: Recoverable issues, unusual situations
- **INFO**: Important business events (creation, deletion, auth)
- **DEBUG**: Detailed execution flow (for development)
- **TRACE**: Very detailed, per-request level

### Sensitive Data

Never log:

- Passwords or password hashes
- JWT tokens or refresh tokens
- Personal identifiable information (PII) in production
- File contents

## Testing Guidelines

### Unit Tests

Test business logic in isolation:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_validate_username() {
        assert!(is_valid_username("alice_123"));
        assert!(!is_valid_username("ab"));  // Too short
        assert!(!is_valid_username("a@b"));  // Invalid char
    }
}
```

### Integration Tests

Use the `capsule-api-testing` crate for database-backed tests:

```rust
#[tokio::test]
async fn test_create_user() {
    let db = testing::setup_db().await;
    
    let result = UserService::create_user(&db, CreateUserArgs { ... }).await;
    assert!(result.is_ok());
    
    // Verify in database
    let user = UserService::find_by_id(&db, &result.unwrap().id).await;
    assert!(user.is_some());
}
```

## Security Practices

### General Guidelines

1. **Input Validation**: Validate all user input at the network layer
2. **Authorization**: Check permissions before every operation
3. **Rate Limiting**: Apply rate limits to authentication and resource-intensive endpoints
4. **Parameterized Queries**: SeaORM handles this, but be careful with raw SQL
5. **Secret Management**: Use `SecretString` for sensitive data, never log tokens
6. **Limit Dependencies:** Only depend on the minimum number of crates necessary. This specifically includes:
   * `sea_orm` code should exist only in `capsule-api-entity`, `capsule-api-migration`, `capsule-api-service`, `capsule-api-testing`
   * ID generation of any sort (e.q., `uuid`, `nanoid`) should exist only in `capsule-api-entity`, `capsule-api-service`

### Dependency Hierarchy

To ease auditing sensitive dependencies/crates, we enforce the following hierarchy of crates (amongst the API crates) (from least to most sensitive):

```plaintext
capsule-api
capsule-api-library; capsule-api-media; capsule-api-sync; capsule-api-upload; capsule-api-auth
capsule-api-service; capsule-api-model; capsule-api-environment
capsule-api-entity; capsule-api-migration

# Omitted: capsule-api-environment, capsule-api-testing
```

The crates in each line must only at most depend on the crate in the same line or the next line. Additionally some of the crates have feature flags guarding certain functionality strictly to certain crates (e.g. `auth` feature in `capsule-api-service` for `capsule-api-auth`).

Note: Some crates in `capsule-api` may have been not mentioned here so use some judgement.
