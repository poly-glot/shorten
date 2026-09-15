use std::env;
use std::fs;
use std::path::Path;
use std::process;

#[path = "src/bundle.rs"]
mod bundle;

const FRONTEND: &str = "../../frontend";
const OUT_DIR_ENV: &str = "OUT_DIR";
const PAGE: &str = "index.html";
const PAGE_BUDGET_BYTES: usize = 50 * 1024;

fn fail(message: &str) -> ! {
    eprintln!("frontend: {message}");
    process::exit(1);
}

fn main() {
    println!("cargo:rerun-if-changed={FRONTEND}");

    let page = match bundle::bundle(Path::new(FRONTEND)) {
        Ok(page) => page,
        Err(error) => fail(&error.to_string()),
    };

    if page.len() > PAGE_BUDGET_BYTES {
        fail(&format!("the inlined page is {} bytes, over the {PAGE_BUDGET_BYTES}-byte budget", page.len()));
    }

    let Ok(out_dir) = env::var(OUT_DIR_ENV) else {
        fail("OUT_DIR is not set; build through cargo");
    };

    if let Err(error) = fs::write(Path::new(&out_dir).join(PAGE), page) {
        fail(&format!("could not write the inlined page: {error}"));
    }
}
