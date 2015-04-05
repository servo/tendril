#![deny(warnings)]

extern crate gcc;

fn main() {
    gcc::Config::new()
        .file("src/glue.c")
        .flag("-O3").flag("-fPIC")
        .include("include")
        .compile("libtendril_cglue.a");
}
