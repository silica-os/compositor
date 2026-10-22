//! The bindings are generated from the XML files by the wayland-scanner macros, which do not
//! tell cargo to rebuild when an XML file changes: this script does.

fn main() {
    println!("cargo:rerun-if-changed=xml");
}
