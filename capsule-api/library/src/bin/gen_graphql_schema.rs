use library::loaders::Loaders;
use library::schema::create_schema;
use sea_orm::DatabaseConnection;

#[tokio::main]
async fn main() {
    let conn = DatabaseConnection::Disconnected;

    let loaders = Loaders::new(conn);
    let schema = create_schema(loaders);
    println!("{}", schema.sdl());
}
