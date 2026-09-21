pub mod formatter;
pub mod parser;

pub fn render_record(input: &str) -> String {
    formatter::render(&parser::parse(input))
}
