fn main() {
    glib_build_tools::compile_resources(
        &["assets/icons"],
        "assets/icons/icons.gresource.xml",
        "icons.gresource",
    );
}
