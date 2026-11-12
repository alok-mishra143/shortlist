//! Isolated PDF text extractor.
//!
//! `pdf-extract` can panic on malformed files. Running it in this helper keeps
//! the GUI process alive: a bad PDF kills this process, and the caller reports
//! a normal parse error.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: shortlist-pdf <path>");
        std::process::exit(2);
    };
    match pdf_extract::extract_text(&path) {
        Ok(text) => print!("{text}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
