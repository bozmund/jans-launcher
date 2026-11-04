fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Jan's Launcher");
        res.set("FileDescription", "Jan's Launcher");
        if std::path::Path::new("assets/icon.ico").exists() {
            res.set_icon("assets/icon.ico");
        }
        res.compile().expect("windows resources");
    }
}
