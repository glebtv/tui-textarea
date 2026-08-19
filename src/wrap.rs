#[cfg(feature = "arbitrary")]
use arbitrary::Arbitrary;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Specify how logical lines are soft-wrapped at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(Arbitrary))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum WrapMode {
    /// Disable soft wrapping and keep horizontal scrolling behavior.
    None,
    /// Wrap only at word boundaries. Words wider than viewport are not split.
    Word,
    /// Wrap at grapheme boundaries.
    Glyph,
    /// Wrap at word boundaries, and fall back to grapheme wrapping for long words.
    WordOrGlyph,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WrappedLine {
    pub row: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub first_in_row: bool,
    pub last_in_row: bool,
}

#[derive(Clone, Copy)]
struct Chunk {
    start: usize,
    end: usize,
}

pub(crate) fn effective_wrap_width(total_width: u16, line_number_len: Option<u8>) -> usize {
    let total_width = total_width as usize;
    let reserved = line_number_len.map(|len| len as usize + 2).unwrap_or(0);
    if total_width > reserved {
        total_width - reserved
    } else {
        1
    }
}

pub(crate) fn wrapped_rows(
    lines: &[String],
    mode: WrapMode,
    width: usize,
    tab_len: u8,
) -> Vec<WrappedLine> {
    let mut rows = Vec::new();

    for (row, line) in lines.iter().enumerate() {
        let ranges = line_ranges(line, mode, width, tab_len);
        for (i, (start_byte, end_byte)) in ranges.iter().copied().enumerate() {
            // Column bookkeeping is byte-accurate: `start_col` is the data column of the
            // fragment's first char and `end_col` the column just past its last char.
            // Whitespace that word-wrap dropped (it never fits a row) is not claimed by
            // any row, so positions inside it have no screen representation.
            let start_col = line[..start_byte].chars().count();
            let end_col = start_col + line[start_byte..end_byte].chars().count();
            rows.push(WrappedLine {
                row,
                start_byte,
                end_byte,
                start_col,
                end_col,
                first_in_row: i == 0,
                last_in_row: i + 1 == ranges.len(),
            });
        }
    }

    rows
}

pub(crate) fn line_ranges(
    line: &str,
    mode: WrapMode,
    width: usize,
    tab_len: u8,
) -> Vec<(usize, usize)> {
    if mode == WrapMode::None {
        return vec![(0, line.len())];
    }

    let width = width.max(1);
    let mut out = match mode {
        WrapMode::None => vec![(0, line.len())],
        WrapMode::Glyph => {
            let mut chunks = Vec::new();
            split_range_by_grapheme_width(line, 0, line.len(), width, tab_len, &mut chunks);
            chunks
        }
        WrapMode::Word => wrap_word_chunks(line, width, tab_len, false),
        WrapMode::WordOrGlyph => wrap_word_chunks(line, width, tab_len, true),
    };

    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

fn wrap_word_chunks(
    line: &str,
    width: usize,
    tab_len: u8,
    fallback_to_glyph: bool,
) -> Vec<(usize, usize)> {
    let chunks: Vec<_> = UnicodeSegmentation::split_word_bound_indices(line)
        .map(|(start, text)| Chunk {
            start,
            end: start + text.len(),
        })
        .collect();

    if chunks.is_empty() {
        return vec![(0, 0)];
    }

    let mut out = Vec::new();
    let mut i = 0usize;
    let mut seg_start = chunks[0].start;
    let mut seg_end = seg_start;
    let mut seg_width = 0usize;

    while i < chunks.len() {
        let chunk = chunks[i];
        if seg_end == seg_start {
            seg_start = chunk.start;
        }

        let chunk_width = display_width_from(chunk_text(line, chunk), seg_width, tab_len);
        if seg_width + chunk_width <= width {
            seg_end = chunk.end;
            seg_width += chunk_width;
            i += 1;
            continue;
        }

        if seg_end > seg_start {
            out.push((seg_start, seg_end));

            // Separator whitespace that does not fit on the current segment is
            // handled like ratatui's WordWrapper (trim: false): when the line
            // is exactly full the first whitespace grapheme is dropped,
            // otherwise whitespace filling the remaining space is dropped;
            // any leftover whitespace leads the next row. This prevents
            // whitespace-only phantom rows (e.g. "abcde fghij" at width 5 must
            // wrap to 2 rows, not 3).
            let text = chunk_text(line, chunk);
            if !text.is_empty() && text.chars().all(char::is_whitespace) {
                let drop_cells = if seg_width == width {
                    1
                } else {
                    width - seg_width
                };
                let kept_start = drop_leading_whitespace(line, chunk, drop_cells, tab_len);
                if kept_start >= chunk.end {
                    // Entire separator dropped: start the next row fresh.
                    seg_start = chunk.end;
                    seg_end = chunk.end;
                    seg_width = 0;
                    i += 1;
                    continue;
                }
                // Leftover whitespace starts the next row.
                let kept_width = display_width_from(&line[kept_start..chunk.end], 0, tab_len);
                if kept_width <= width {
                    seg_start = kept_start;
                    seg_end = chunk.end;
                    seg_width = kept_width;
                    i += 1;
                    continue;
                }
                // Pathological: leftover whitespace wider than the row.
                if fallback_to_glyph {
                    split_range_by_grapheme_width(
                        line, kept_start, chunk.end, width, tab_len, &mut out,
                    );
                } else {
                    out.push((kept_start, chunk.end));
                }
                i += 1;
                seg_start = chunk.end;
                seg_end = chunk.end;
                seg_width = 0;
                continue;
            }

            seg_start = seg_end;
            seg_width = 0;
            continue;
        }

        if fallback_to_glyph {
            split_range_by_grapheme_width(line, chunk.start, chunk.end, width, tab_len, &mut out);
        } else {
            out.push((chunk.start, chunk.end));
        }

        i += 1;
        seg_start = chunk.end;
        seg_end = chunk.end;
        seg_width = 0;
    }

    if seg_end > seg_start {
        out.push((seg_start, seg_end));
    }

    out
}

fn split_range_by_grapheme_width(
    line: &str,
    start: usize,
    end: usize,
    width: usize,
    tab_len: u8,
    out: &mut Vec<(usize, usize)>,
) {
    let mut segment_start = start;
    while segment_start < end {
        let mut segment_end = segment_start;
        let mut segment_width = 0usize;

        for (offset, grapheme) in
            UnicodeSegmentation::grapheme_indices(&line[segment_start..end], true)
        {
            let grapheme_start = segment_start + offset;
            let grapheme_end = grapheme_start + grapheme.len();
            let next_width = display_width_to(grapheme, segment_width, tab_len);
            let grapheme_width = next_width.saturating_sub(segment_width);

            if segment_end != segment_start && segment_width + grapheme_width > width {
                break;
            }

            segment_end = grapheme_end;
            segment_width = next_width;
            if segment_width > width {
                break;
            }
        }

        if segment_end == segment_start {
            if let Some(ch) = line[segment_start..end].chars().next() {
                segment_end = segment_start + ch.len_utf8();
            } else {
                break;
            }
        }

        out.push((segment_start, segment_end));
        segment_start = segment_end;
    }
}

#[inline]
fn chunk_text(line: &str, chunk: Chunk) -> &str {
    &line[chunk.start..chunk.end]
}

/// Return the byte offset inside `chunk` after dropping up to `drop_cells`
/// display cells of leading whitespace (grapheme-atomic). Used to trim
/// separator whitespace at line breaks like ratatui's `WordWrapper` does.
fn drop_leading_whitespace(line: &str, chunk: Chunk, drop_cells: usize, tab_len: u8) -> usize {
    let text = &line[chunk.start..chunk.end];
    let mut col = 0usize;
    let mut cells = 0usize;
    let mut offset = 0usize;
    for (grapheme_offset, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
        let width = display_width_to(grapheme, col, tab_len).saturating_sub(col);
        if width == 0 {
            continue;
        }
        if cells + width > drop_cells {
            break;
        }
        cells += width;
        col += width;
        offset = grapheme_offset + grapheme.len();
    }
    chunk.start + offset
}

fn display_width_from(text: &str, start_width: usize, tab_len: u8) -> usize {
    display_width_to(text, start_width, tab_len).saturating_sub(start_width)
}

fn display_width_to(text: &str, mut width: usize, tab_len: u8) -> usize {
    for c in text.chars() {
        if c == '\t' {
            if tab_len > 0 {
                let tab = tab_len as usize;
                let pad = tab - (width % tab);
                width += pad;
            }
        } else {
            width += c.width().unwrap_or(0);
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(line: &str, mode: WrapMode, width: usize) -> Vec<&str> {
        line_ranges(line, mode, width, 4)
            .into_iter()
            .map(|(s, e)| &line[s..e])
            .collect()
    }

    #[test]
    fn word_wrap_keeps_long_word() {
        let have = segments("helloworld", WrapMode::Word, 4);
        assert_eq!(have, vec!["helloworld"]);
    }

    #[test]
    fn word_or_glyph_wrap_splits_long_word() {
        let have = segments("helloworld", WrapMode::WordOrGlyph, 4);
        assert_eq!(have, vec!["hell", "owor", "ld"]);
    }

    #[test]
    fn glyph_wrap_handles_wide_chars() {
        let have = segments("ab犬猫", WrapMode::Glyph, 4);
        assert_eq!(have, vec!["ab犬", "猫"]);
    }

    #[test]
    fn glyph_wrap_keeps_combining_grapheme_cluster() {
        let have = segments("e\u{301}x", WrapMode::Glyph, 1);
        assert_eq!(have, vec!["e\u{301}", "x"]);
    }

    #[test]
    fn tab_width_is_accounted_for_in_wrap() {
        let have = segments("\tX", WrapMode::WordOrGlyph, 2);
        assert_eq!(have, vec!["\t", "X"]);
    }

    #[test]
    fn separator_whitespace_dropped_when_line_exactly_full() {
        // One space after an exactly full word must not produce a phantom row.
        let have = segments("1234567890 ", WrapMode::WordOrGlyph, 10);
        assert_eq!(have, vec!["1234567890"]);
        let have = segments("abcde fghij", WrapMode::WordOrGlyph, 5);
        assert_eq!(have, vec!["abcde", "fghij"]);
        let have = segments("abcde fghij", WrapMode::Word, 5);
        assert_eq!(have, vec!["abcde", "fghij"]);
        let have = segments("abcde fghi", WrapMode::WordOrGlyph, 5);
        assert_eq!(have, vec!["abcde", "fghi"]);
    }

    #[test]
    fn separator_whitespace_overflowing_remaining_space_is_dropped() {
        // "  " after a 4/5 segment: 1 cell fits the remaining space and is
        // kept (leading the next row), the second cell is dropped — no
        // phantom whitespace-only row.
        let have = segments("abcd  efgh", WrapMode::WordOrGlyph, 5);
        assert_eq!(have, vec!["abcd", " efgh"]);
        let have = segments("abcd  efgh", WrapMode::Word, 5);
        assert_eq!(have, vec!["abcd", " efgh"]);
    }

    #[test]
    fn leftover_separator_whitespace_leads_next_row() {
        // After an exactly full word the first separator grapheme is dropped,
        // the rest stays visible at the start of the next row.
        let have = segments("1234567890  ", WrapMode::WordOrGlyph, 10);
        assert_eq!(have, vec!["1234567890", " "]);
        let have = segments("abcde  fghij", WrapMode::WordOrGlyph, 5);
        assert_eq!(have, vec!["abcde", " ", "fghij"]);
    }

    #[test]
    fn wrap_matches_ratatui_row_counts() {
        // Row counts must match ratatui's WordWrapper (trim: false) — the same
        // algorithm the TUI renders with. `ratatui_reference_rows` is a faithful
        // port of ratatui's WordWrapper::process_input; `ratatui_buffer_rows`
        // cross-checks it against the real Paragraph renderer for inputs that do
        // not end in a whitespace-only row (those are indistinguishable from
        // empty buffer space).
        let cases: &[(&str, usize)] = &[
            ("1234567890", 10),
            ("1234567890 ", 10),
            ("1234567890 x", 10),
            ("1234567890  ", 10),
            ("1234567890   ", 10),
            ("hello world", 10),
            ("hello worldx", 10),
            ("  padded  text  ", 8),
            ("abcde fghij", 5),
            ("abcde fghi", 5),
            ("abcde fgh", 5),
            ("abcde  fghij", 5),
            ("abcd  efgh", 5),
            ("abcd    efgh", 5),
            ("a b c d e f g h i j k l", 5),
            ("ab cd ef gh", 5),
            ("x y z", 2),
            ("word word word", 8),
            ("12345678901234567890", 10),
            ("123456789012345678901", 10),
            ("第二行 mixed-width 内容 with English words.", 18),
            ("ASCII and русский текст can share one buffer.", 18),
            (
                "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
                24,
            ),
        ];
        for (text, width) in cases {
            let ours = segments(text, WrapMode::WordOrGlyph, *width).len();
            let reference = ratatui_reference_rows(text, *width);
            assert_eq!(
                ours, reference,
                "row count mismatch for {text:?} at width {width}: fork={ours} \
                 ratatui(reference)={reference}"
            );
            if !text.ends_with(|c: char| c.is_whitespace()) {
                // Buffer rendering cannot expose whitespace-only trailing rows.
                let buffer_rows = ratatui_buffer_rows(text, *width);
                assert_eq!(
                    reference, buffer_rows,
                    "reference diverges from real ratatui render for {text:?} at {width}"
                );
            }
        }
    }

    /// Faithful port of ratatui's `WordWrapper::process_input` (trim: false).
    /// Returns the number of wrapped lines ratatui would render.
    fn ratatui_reference_rows(text: &str, width: usize) -> usize {
        let width = width as u16;
        let mut wrapped: Vec<Vec<String>> = Vec::new();
        let mut line: Vec<String> = Vec::new();
        let mut line_width = 0u16;
        let mut pending_word: Vec<String> = Vec::new();
        let mut word_width = 0u16;
        let mut pending_ws: Vec<String> = Vec::new();
        let mut ws_width = 0u16;
        let mut non_ws_prev = true;

        let symbols: Vec<(String, u16)> =
            unicode_segmentation::UnicodeSegmentation::graphemes(text, true)
                .map(|s| {
                    (
                        s.to_string(),
                        unicode_width::UnicodeWidthStr::width(s) as u16,
                    )
                })
                .collect();
        for (symbol, symbol_width) in symbols {
            let is_whitespace = symbol.trim().is_empty() && symbol_width > 0;
            if symbol_width > width {
                continue;
            }
            let word_found = non_ws_prev && is_whitespace;
            let trimmed_overflow = line.is_empty() && word_width + symbol_width > width;
            let whitespace_overflow = line.is_empty() && ws_width + symbol_width > width;
            let untrimmed_overflow =
                line.is_empty() && word_width + ws_width + symbol_width > width;
            if word_found || trimmed_overflow || whitespace_overflow || untrimmed_overflow {
                // Port is used with trim: false only, where ratatui always extends
                // pending whitespace into the line (`!line.is_empty() || !trim`).
                line.append(&mut pending_ws);
                line_width += ws_width;
                line.append(&mut pending_word);
                line_width += word_width;
                pending_ws.clear();
                ws_width = 0;
                word_width = 0;
            }
            let line_full = line_width >= width;
            let pending_word_overflow =
                symbol_width > 0 && line_width + ws_width + word_width >= width;
            if line_full || pending_word_overflow {
                let mut remaining = width.saturating_sub(line_width);
                if !line.is_empty() {
                    wrapped.push(std::mem::take(&mut line));
                }
                line_width = 0;
                while let Some(first) = pending_ws.first() {
                    let w = unicode_width::UnicodeWidthStr::width(first.as_str()) as u16;
                    if w > remaining {
                        break;
                    }
                    ws_width -= w;
                    remaining -= w;
                    pending_ws.remove(0);
                }
                if is_whitespace && pending_ws.is_empty() {
                    non_ws_prev = !is_whitespace;
                    continue;
                }
            }
            if is_whitespace {
                ws_width += symbol_width;
                pending_ws.push(symbol);
            } else {
                word_width += symbol_width;
                pending_word.push(symbol);
            }
            non_ws_prev = !is_whitespace;
        }
        // End of input: ratatui appends pending whitespace (and the pending word)
        // to the line even when trim is false and the line is empty.
        line.append(&mut pending_ws);
        line.append(&mut pending_word);
        if !line.is_empty() {
            wrapped.push(line);
        }
        wrapped.len().max(1)
    }

    fn ratatui_buffer_rows(text: &str, width: usize) -> usize {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::text::Text;
        use ratatui::widgets::{Paragraph, Wrap};

        let backend = TestBackend::new(width as u16, 100);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|f| {
                f.render_widget(
                    Paragraph::new(Text::from(text.to_string())).wrap(Wrap { trim: false }),
                    f.area(),
                );
            })
            .expect("draw should succeed");
        let buffer = terminal.backend().buffer().clone();
        let mut last_non_blank = None;
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            if !line.trim().is_empty() {
                last_non_blank = Some(y);
            }
        }
        match last_non_blank {
            Some(y) => y as usize + 1,
            None => 1,
        }
    }
}
