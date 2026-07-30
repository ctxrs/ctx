use crate::ui::{Document, Line, Token};

pub(crate) fn section(title: &str, body: Document) -> Document {
    let mut document = Document::from_line(Line::styled(title, Token::Heading));
    document.append(body);
    document
}
