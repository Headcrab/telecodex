use std::env;

fn main() {
    let number = env::var("TELECODEX_BUILD_NUMBER").unwrap_or_else(|_| "0".to_string());
    let number: u64 = number
        .parse()
        .expect("TELECODEX_BUILD_NUMBER must be an integer");
    println!("cargo:rerun-if-env-changed=TELECODEX_BUILD_NUMBER");
    println!("cargo:rustc-env=TELECODEX_BUILD_NUMBER={number}");
    println!(
        "cargo:rustc-env=TELECODEX_APP_VERSION={}",
        env::var("CARGO_PKG_VERSION").expect("pkg version")
    );
}
