use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // visualize-allocatorフィーチャーが有効な場合、マーカーファイルを作成
    let out_dir = env::var("OUT_DIR").unwrap();
    let marker_path = Path::new(&out_dir)
        .join("../../..")
        .join("VISUALIZE_ENABLED");

    #[cfg(feature = "visualize-allocator")]
    {
        fs::write(&marker_path, "1").unwrap();
        println!("cargo:warning=Visualization feature enabled");
    }

    #[cfg(not(feature = "visualize-allocator"))]
    {
        let _ = fs::remove_file(&marker_path);
    }
}
