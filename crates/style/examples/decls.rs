//! Print every declaration a stylesheet parses to (for comparing parser
//! versions): `cargo run -p style --example decls -- a.css b.css`
fn main() {
    for path in std::env::args().skip(1) {
        let css = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        let sheet = style::sheet::parse_stylesheet(&css);
        for rule in &sheet.rules {
            println!("{:?} {:?} | {:?}", rule.media, rule.normal, rule.important);
        }
    }
}
