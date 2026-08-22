fn main() {
    for asset in [
        "../../../hydra-ui/app.js",
        "../../../hydra-ui/hydra-icon.png",
        "../../../hydra-ui/index.html",
        "../../../hydra-ui/model.js",
        "../../../hydra-ui/styles.css",
        "../../../hydra-ui/theme.js",
    ] {
        println!("cargo:rerun-if-changed={asset}");
    }
    tauri_build::build();
}
