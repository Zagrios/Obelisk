use std::path::Path;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("Usage: sailover-steam-runtime-package DOWNLOAD_CACHE OUTPUT_ZIP".into());
    }
    game_runtime::packaging::build(Path::new(&args[1]), Path::new(&args[2])).await
}
