use std::env;

fn main() {
    let _ = dotenvy::dotenv();

    if let Ok(api_url) = env::var("DAWELLSERVICE_API_BASE_URL") {
        println!("cargo:rustc-env=DAWELLSERVICE_API_BASE_URL={}", api_url);
    }

    if let Ok(ws_url) = env::var("DAWELLSERVICE_WS_URL") {
        println!("cargo:rustc-env=DAWELLSERVICE_WS_URL={}", ws_url);
    }

    println!("cargo:rerun-if-changed=.env");
}
