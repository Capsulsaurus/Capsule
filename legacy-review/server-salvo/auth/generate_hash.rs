use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString},
};
use rand_core::OsRng;

fn main() {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password("dummy_password".as_bytes(), &salt)
        .unwrap()
        .to_string();
    println!("{}", password_hash);
}
