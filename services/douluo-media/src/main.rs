#[tokio::main]
async fn main() {
    if let Err(error) = douluo_media::run_from_env().await {
        eprintln!("douluo-media: {error}");
        std::process::exit(1);
    }
}
