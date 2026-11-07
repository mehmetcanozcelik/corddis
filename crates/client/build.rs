//! Windows'ta exe dosyasına ikon ve sürüm bilgisi gömer (Gezgin'de ve görev çubuğunda görünür).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Corddis");
        res.set("FileDescription", "Corddis");
        if let Err(e) = res.compile() {
            // İkon gömülemese de uygulama derlensin.
            println!("cargo:warning=ikon gömülemedi: {e}");
        }
    }
}
