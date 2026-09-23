//! Bounded text traversal; PDF syntax and character decoding belong to lopdf.
use super::IngestOptions;
use anyhow::{Context, Result, bail, ensure};
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId, content::Content};
use std::{collections::HashMap, rc::Rc};

const MAX_DEPTH: usize = 64;
type Matrix = [f32; 6];
const IDENTITY: Matrix = [1., 0., 0., 1., 0., 0.];

fn supported_encoding(name: &[u8]) -> Result<()> {
    ensure!(
        matches!(
            name,
            b"StandardEncoding"
                | b"MacRomanEncoding"
                | b"MacExpertEncoding"
                | b"WinAnsiEncoding"
                | b"PDFDocEncoding"
                | b"UniGB-UCS2-H"
                | b"UniGB-UTF16-H"
        ),
        "unsupported PDF font encoding"
    );
    Ok(())
}

fn implicit_encoding(font: &Dictionary) -> Result<()> {
    // Symbol/ZapfDingbats and custom fonts cannot use a guessed StandardEncoding.
    ensure!(
        matches!(
            font.get(b"BaseFont")?.as_name()?,
            b"Helvetica"
                | b"Helvetica-Bold"
                | b"Helvetica-Oblique"
                | b"Helvetica-BoldOblique"
                | b"Courier"
                | b"Courier-Bold"
                | b"Courier-Oblique"
                | b"Courier-BoldOblique"
                | b"Times-Roman"
                | b"Times-Bold"
                | b"Times-Italic"
                | b"Times-BoldItalic"
        ),
        "PDF font needs an explicit supported encoding or ToUnicode map"
    );
    Ok(())
}

fn transform(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

#[derive(Clone)]
struct Graphics<'a> {
    font: Option<Rc<Encoding<'a>>>,
    leading: f32,
    matrix: Matrix,
}

impl Default for Graphics<'_> {
    fn default() -> Self {
        Self {
            font: None,
            leading: 0.,
            matrix: IDENTITY,
        }
    }
}

pub(super) fn extract(bytes: &[u8], options: &IngestOptions) -> Result<String> {
    let limit = options.max_input_bytes as usize;
    let document = Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_max_decompressed_size(limit),
    )
    .context("cannot load PDF document")?;
    let mut walker = Walker {
        document: &document,
        remaining: limit,
        stream_limit: limit,
        text_limit: options.max_text_bytes,
        text: String::new(),
        baseline: None,
        active_forms: Vec::new(),
        fonts: HashMap::new(),
    };
    for (_, page) in document.get_pages() {
        let resources = walker.page_resources(page)?;
        let content = document.get_page_content_with_limit(page, walker.remaining)?;
        walker.walk(&content, resources, Graphics::default(), IDENTITY, 0)?;
        walker.newline()?;
    }
    Ok(walker.text)
}

struct Walker<'a> {
    document: &'a Document,
    remaining: usize,
    stream_limit: usize,
    text_limit: usize,
    text: String,
    baseline: Option<Matrix>,
    active_forms: Vec<ObjectId>,
    // Dictionaries have stable addresses for this immutable document's lifetime.
    fonts: HashMap<*const Dictionary, Rc<Encoding<'a>>>,
}

impl<'a> Walker<'a> {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .context("PDF decoded-content byte limit exceeded")?;
        Ok(())
    }

    fn append(&mut self, text: &str) -> Result<()> {
        ensure!(
            text.len() <= self.text_limit.saturating_sub(self.text.len()),
            "converted document exceeds text byte limit"
        );
        self.text.push_str(text);
        Ok(())
    }

    fn newline(&mut self) -> Result<()> {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.append("\n")?;
        }
        self.baseline = None;
        Ok(())
    }

    fn page_resources(&self, mut page: ObjectId) -> Result<Option<&'a Dictionary>> {
        let mut visited = Vec::new();
        loop {
            ensure!(
                visited.len() < MAX_DEPTH,
                "PDF resource nesting limit exceeded"
            );
            ensure!(!visited.contains(&page), "PDF resource cycle");
            visited.push(page);
            let dictionary = self.document.get_dictionary(page)?;
            if dictionary.has(b"Resources") {
                return Ok(Some(
                    dictionary
                        .get_deref(b"Resources", self.document)?
                        .as_dict()?,
                ));
            }
            if !dictionary.has(b"Parent") {
                return Ok(None);
            }
            page = dictionary.get(b"Parent")?.as_reference()?;
        }
    }

    fn resource(
        &self,
        resources: Option<&'a Dictionary>,
        kind: &[u8],
        name: &[u8],
    ) -> Result<(Option<ObjectId>, &'a Object)> {
        let table = resources
            .context("PDF content has no resources")?
            .get_deref(kind, self.document)?
            .as_dict()?;
        Ok(self.document.dereference(table.get(name)?)?)
    }

    fn font(&mut self, font: &'a Dictionary) -> Result<Rc<Encoding<'a>>> {
        let key = std::ptr::from_ref(font);
        if let Some(encoding) = self.fonts.get(&key) {
            return Ok(encoding.clone());
        }
        // ToUnicode is authoritative even when a base Encoding is also supplied.
        let encoding = if font.has(b"ToUnicode") {
            let stream = font.get_deref(b"ToUnicode", self.document)?.as_stream()?;
            let decoded = stream.get_plain_content_with_limit(self.remaining)?;
            self.charge(decoded.len())?;
            let mut unicode_font = font.clone();
            unicode_font.remove(b"Encoding");
            match unicode_font.get_font_encoding_with_limit(self.document, self.stream_limit)? {
                Encoding::UnicodeMapEncoding(cmap) => Encoding::UnicodeMapEncoding(cmap),
                _ => bail!("invalid or unsupported PDF ToUnicode encoding"),
            }
        } else {
            let encoding = font.get_font_encoding_with_limit(self.document, self.stream_limit)?;
            if font.has(b"Encoding") {
                let declared = font.get_deref(b"Encoding", self.document)?;
                match declared {
                    Object::Dictionary(dictionary) => {
                        if dictionary.has(b"BaseEncoding") {
                            // lopdf falls back to StandardEncoding for a non-name base.
                            supported_encoding(dictionary.get(b"BaseEncoding")?.as_name()?)?;
                        } else {
                            implicit_encoding(font)?;
                        }
                        ensure!(
                            matches!(encoding, Encoding::Differences(_)),
                            "invalid or unsupported PDF font encoding dictionary"
                        );
                    }
                    Object::Name(name) => supported_encoding(name)?,
                    _ => bail!("invalid PDF font encoding"),
                }
            } else {
                implicit_encoding(font)?;
            }
            encoding
        };
        let encoding = Rc::new(encoding);
        self.fonts.insert(key, encoding.clone());
        Ok(encoding)
    }

    fn show(&mut self, object: &Object, graphics: &Graphics<'a>, line: Matrix) -> Result<()> {
        let font = graphics
            .font
            .as_ref()
            .context("PDF text has no selected font")?;
        let position = transform(graphics.matrix, line);
        ensure!(
            position.iter().all(|v| v.is_finite()),
            "invalid PDF text position"
        );
        if let Some(previous) = self.baseline {
            let dx = position[4] - previous[4];
            let dy = position[5] - previous[5];
            // Compare displacement perpendicular to the previous baseline. Horizontal
            // repositioning alone must not split ordinary same-line text fragments.
            let length = previous[0].hypot(previous[1]);
            if (dx * previous[1] - dy * previous[0]).abs() > 0.01 * length
                || position[..4] != previous[..4]
            {
                self.newline()?;
            }
        }
        self.baseline = Some(position);
        // lopdf allocates one decoded operand before we can enforce the text cap.
        let decoded = Document::decode_text(font, object.as_str()?)?;
        self.append(&decoded)
    }

    fn walk(
        &mut self,
        bytes: &[u8],
        resources: Option<&'a Dictionary>,
        mut graphics: Graphics<'a>,
        mut line: Matrix,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth < MAX_DEPTH, "PDF Form nesting limit exceeded");
        self.charge(bytes.len())?;
        let content = Content::decode_strict(bytes).context("invalid PDF content stream")?;
        let mut stack = Vec::new();
        for operation in content.operations {
            let args = &operation.operands;
            let arg = |i: usize| args.get(i).context("missing PDF operator operand");
            let number = |i: usize| -> Result<f32> {
                let value = arg(i)?.as_float()?;
                ensure!(value.is_finite(), "invalid PDF numeric operand");
                Ok(value)
            };
            match operation.operator.as_str() {
                "q" => {
                    ensure!(
                        stack.len() < MAX_DEPTH,
                        "PDF graphics nesting limit exceeded"
                    );
                    stack.push(graphics.clone());
                }
                "Q" => graphics = stack.pop().context("unbalanced PDF graphics restore")?,
                "cm" => {
                    graphics.matrix = transform(
                        graphics.matrix,
                        [
                            number(0)?,
                            number(1)?,
                            number(2)?,
                            number(3)?,
                            number(4)?,
                            number(5)?,
                        ],
                    )
                }
                "BT" => {
                    line = IDENTITY;
                }
                "ET" => self.newline()?,
                "Tf" => {
                    let (_, font) = self.resource(resources, b"Font", arg(0)?.as_name()?)?;
                    graphics.font = Some(self.font(font.as_dict()?)?);
                    number(1)?;
                }
                "TL" => graphics.leading = number(0)?,
                "Tm" => {
                    line = [
                        number(0)?,
                        number(1)?,
                        number(2)?,
                        number(3)?,
                        number(4)?,
                        number(5)?,
                    ]
                }
                "Td" | "TD" => {
                    let (x, y) = (number(0)?, number(1)?);
                    if operation.operator == "TD" {
                        graphics.leading = -y;
                    }
                    line = transform(line, [1., 0., 0., 1., x, y]);
                }
                "T*" | "'" | "\"" => {
                    line = transform(line, [1., 0., 0., 1., 0., -graphics.leading]);
                    self.newline()?;
                    if operation.operator != "T*" {
                        self.show(
                            arg(if operation.operator == "\"" { 2 } else { 0 })?,
                            &graphics,
                            line,
                        )?;
                    }
                }
                "Tj" => self.show(arg(0)?, &graphics, line)?,
                "TJ" => {
                    for item in arg(0)?.as_array()? {
                        match item {
                            Object::String(_, _) => self.show(item, &graphics, line)?,
                            Object::Integer(_) | Object::Real(_) => {
                                if item.as_float()? < -100. && !self.text.ends_with([' ', '\n']) {
                                    self.append(" ")?;
                                }
                            }
                            _ => bail!("invalid PDF text array operand"),
                        }
                    }
                }
                "Do" => {
                    let (id, object) = self.resource(resources, b"XObject", arg(0)?.as_name()?)?;
                    let form = object.as_stream()?;
                    match form.dict.get(b"Subtype")?.as_name()? {
                        b"Image" => continue,
                        b"Form" => {}
                        _ => bail!("unsupported PDF XObject subtype"),
                    }
                    if let Some(id) = id {
                        ensure!(!self.active_forms.contains(&id), "PDF Form cycle");
                        self.active_forms.push(id);
                    }
                    let form_resources = if form.dict.has(b"Resources") {
                        Some(
                            form.dict
                                .get_deref(b"Resources", self.document)?
                                .as_dict()?,
                        )
                    } else {
                        resources
                    };
                    let mut form_graphics = graphics.clone();
                    if form.dict.has(b"Matrix") {
                        let values = form.dict.get_deref(b"Matrix", self.document)?.as_array()?;
                        ensure!(values.len() == 6, "invalid PDF Form matrix");
                        let mut matrix = IDENTITY;
                        for (slot, value) in matrix.iter_mut().zip(values) {
                            *slot = value.as_float()?;
                        }
                        ensure!(
                            matrix.iter().all(|v| v.is_finite()),
                            "invalid PDF Form matrix"
                        );
                        form_graphics.matrix = transform(graphics.matrix, matrix);
                    }
                    let decoded = form.get_plain_content_with_limit(self.remaining)?;
                    self.walk(&decoded, form_resources, form_graphics, line, depth + 1)?;
                    if id.is_some() {
                        self.active_forms.pop();
                    }
                }
                _ => {}
            }
        }
        ensure!(stack.is_empty(), "unbalanced PDF graphics save");
        Ok(())
    }
}
