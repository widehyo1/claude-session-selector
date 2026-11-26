use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
};

pub(crate) fn pane_content_area(area: Rect) -> Rect {
    Block::new().borders(Borders::ALL).inner(area)
}

pub(crate) fn half_page_height(height: u16) -> u16 {
    (height / 2).max(1)
}

pub(crate) fn bottom_scroll_offset(line_count: usize, viewport_height: u16) -> u16 {
    let offset = line_count.saturating_sub(usize::from(viewport_height));
    offset.min(usize::from(u16::MAX)) as u16
}

// Pads or cuts `value` to exactly `width` terminal cells, marking a cut with `…`.
pub(crate) fn fit_width(value: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let value_width = UnicodeWidthStr::width(value);
    if value_width <= width {
        return format!("{value}{}", " ".repeat(width - value_width));
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width > width - 1 {
            break;
        }
        result.push(ch);
        used += char_width;
    }
    result.push('…');
    result.push_str(&" ".repeat(width - used - 1));
    result
}

/// Like [`fit_width`], but truncates from the front so the tail stays visible.
pub(crate) fn fit_width_tail(value: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let value_width = UnicodeWidthStr::width(value);
    if value_width <= width {
        return format!("{value}{}", " ".repeat(width - value_width));
    }
    if width == 0 {
        return String::new();
    }
    let mut tail = Vec::new();
    let mut used = 0;
    for ch in value.chars().rev() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width > width - 1 {
            break;
        }
        tail.push(ch);
        used += char_width;
    }
    let mut result = String::from("…");
    result.extend(tail.into_iter().rev());
    result.push_str(&" ".repeat(width - used - 1));
    result
}

#[cfg(test)]
mod fit_width_tail_tests {
    use super::fit_width_tail;

    #[test]
    fn keeps_the_tail_and_pads_to_width() {
        assert_eq!(
            fit_width_tail("/Users/me/gitclone/project", 12),
            "…one/project"
        );
        assert_eq!(fit_width_tail("short", 8), "short   ");
        assert_eq!(fit_width_tail("feat/한글브랜치", 8), "…브랜치 ");
    }
}
