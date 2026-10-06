fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/riptide.ico");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/riptide.ico").set("ProductName", "Riptide").set("FileDescription", "Riptide");
        if let Err(e) = resource.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
