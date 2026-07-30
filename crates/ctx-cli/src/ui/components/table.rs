use super::layout::{display_width, pad, pad_after, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

const COLUMN_GAP: usize = 2;
const MIN_WIDE_WIDTH: usize = 60;
const MIN_COLUMN_WIDTH: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub(crate) fn new<I, S>(columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    pub(crate) fn push_row<I, S>(&mut self, row: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(row.into_iter().map(Into::into).collect());
    }

    pub(crate) fn row<I, S>(mut self, row: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.push_row(row);
        self
    }

    pub(crate) fn columns(&self) -> &[String] {
        &self.columns
    }

    pub(crate) fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }
}

pub(crate) fn table(context: &RenderContext, table: &Table) -> Document {
    if table.columns.is_empty() {
        return Document::new();
    }

    let natural = natural_widths(table);
    let minimum = minimum_widths(table);
    let gap_width = COLUMN_GAP.saturating_mul(table.columns.len().saturating_sub(1));
    let minimum_total = minimum
        .iter()
        .copied()
        .sum::<usize>()
        .saturating_add(gap_width);

    let should_stack = context
        .content_width()
        .is_some_and(|width| width < MIN_WIDE_WIDTH || width < minimum_total);
    if should_stack {
        return stacked_table(context, table);
    }

    let widths = match context.content_width() {
        Some(available) => shrink_widths(natural, &minimum, available.saturating_sub(gap_width)),
        None => natural,
    };
    wide_table(table, &widths)
}

fn natural_widths(table: &Table) -> Vec<usize> {
    table
        .columns
        .iter()
        .enumerate()
        .map(|(column, heading)| {
            table
                .rows
                .iter()
                .fold(display_width(heading), |width, row| {
                    row.get(column)
                        .map_or(width, |value| width.max(display_width(value)))
                })
        })
        .collect()
}

fn minimum_widths(table: &Table) -> Vec<usize> {
    table
        .columns
        .iter()
        .map(|heading| display_width(heading).max(MIN_COLUMN_WIDTH))
        .collect()
}

fn shrink_widths(mut widths: Vec<usize>, minimum: &[usize], available: usize) -> Vec<usize> {
    let mut overflow = widths.iter().sum::<usize>().saturating_sub(available);
    while overflow > 0 {
        let adjustable = widths
            .iter()
            .zip(minimum)
            .filter(|(width, minimum)| width > minimum)
            .count();
        if adjustable == 0 {
            break;
        }
        let share = overflow.saturating_add(adjustable - 1) / adjustable;
        let mut reduced = 0;
        for (width, minimum) in widths.iter_mut().zip(minimum) {
            let reduction = width.saturating_sub(*minimum).min(share);
            *width = width.saturating_sub(reduction);
            reduced += reduction;
        }
        if reduced == 0 {
            break;
        }
        overflow = overflow.saturating_sub(reduced);
    }
    widths
}

fn wide_table(table: &Table, widths: &[usize]) -> Document {
    let mut document = Document::new();
    let headings = table
        .columns
        .iter()
        .map(|heading| vec![heading.clone()])
        .collect::<Vec<_>>();
    push_visual_row(&mut document, &headings, widths, Token::Heading);

    for row in &table.rows {
        let cells = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                wrap_text(row.get(column).map_or("", String::as_str), Some(*width))
            })
            .collect::<Vec<_>>();
        push_visual_row(&mut document, &cells, widths, Token::Text);
    }
    document
}

fn push_visual_row(document: &mut Document, cells: &[Vec<String>], widths: &[usize], token: Token) {
    let height = cells.iter().map(Vec::len).max().unwrap_or(1);
    for visual_line in 0..height {
        let last_content = cells
            .iter()
            .enumerate()
            .rev()
            .find(|(_, cell)| cell.get(visual_line).is_some_and(|value| !value.is_empty()))
            .map(|(column, _)| column)
            .unwrap_or(0);
        let mut line = Line::new();
        let visible_columns = last_content
            .min(widths.len().saturating_sub(1))
            .saturating_add(1);
        for (column, width) in widths.iter().enumerate().take(visible_columns) {
            let value = cells
                .get(column)
                .and_then(|cell| cell.get(visual_line))
                .map_or("", String::as_str);
            line.push(Span::new(value, token));
            if column < last_content {
                line.push(Span::text(pad_after(value, *width)));
                line.push(Span::text(pad(COLUMN_GAP)));
            }
        }
        document.push_line(line);
    }
}

fn stacked_table(context: &RenderContext, table: &Table) -> Document {
    let indent = COLUMN_GAP;
    let value_width = context
        .content_width()
        .map(|width| width.saturating_sub(indent).max(1));
    let mut document = Document::new();

    for (row_index, row) in table.rows.iter().enumerate() {
        if row_index > 0 {
            document.push_blank();
        }
        for (column, heading) in table.columns.iter().enumerate() {
            document.push_line(Line::styled(heading, Token::Label));
            for value in wrap_text(row.get(column).map_or("", String::as_str), value_width) {
                document.push_line(
                    Line::new()
                        .with(Span::text(pad(indent)))
                        .with(Span::text(value)),
                );
            }
        }
    }
    document
}
