/// High-contrast ANSI color palette for `gir tui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Reset,
    DarkGray,
    White,
    Green,
    Red,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl Color {
    pub const fn fg_ansi(self) -> &'static str {
        match self {
            Self::Reset => "\x1b[39m",
            Self::DarkGray => "\x1b[90m",
            Self::White => "\x1b[97m",
            Self::Green => "\x1b[32m",
            Self::Red => "\x1b[31m",
            Self::Yellow => "\x1b[33m",
            Self::Blue => "\x1b[34m",
            Self::Magenta => "\x1b[35m",
            Self::Cyan => "\x1b[36m",
        }
    }

    pub const fn bg_ansi(self) -> &'static str {
        match self {
            Self::Reset => "\x1b[49m",
            Self::DarkGray => "\x1b[100m",
            Self::White => "\x1b[107m",
            Self::Green => "\x1b[42m",
            Self::Red => "\x1b[41m",
            Self::Yellow => "\x1b[43m",
            Self::Blue => "\x1b[44m",
            Self::Magenta => "\x1b[45m",
            Self::Cyan => "\x1b[46m",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Style {
    pub const fn fg(fg: Color) -> Self {
        Self {
            fg,
            bg: Color::Reset,
            bold: false,
        }
    }

    pub const fn bold_fg(fg: Color) -> Self {
        Self {
            fg,
            bg: Color::Reset,
            bold: true,
        }
    }

    pub const fn selected() -> Self {
        Self {
            fg: Color::White,
            bg: Color::Blue,
            bold: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn inner(self) -> Self {
        if self.width < 2 || self.height < 2 {
            return Self::new(self.x, self.y, 0, 0);
        }
        Self::new(self.x + 1, self.y + 1, self.width - 2, self.height - 2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

/// Immediate-mode 2D terminal cell buffer for flicker-free rendering and deterministic headless testing.
#[derive(Debug, Clone)]
pub struct ScreenBuffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

impl ScreenBuffer {
    pub fn new(width: u16, height: u16) -> Self {
        let len = (width as usize) * (height as usize);
        Self {
            width,
            height,
            cells: vec![Cell::default(); len],
        }
    }

    pub fn set(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            self.cells[idx] = Cell { ch, style };
        }
    }

    pub fn draw_text(&mut self, x: u16, y: u16, max_width: u16, text: &str, style: Style) {
        if y >= self.height || max_width == 0 {
            return;
        }
        for (i, ch) in text.chars().enumerate() {
            if (i as u16) >= max_width {
                break;
            }
            let cx = x + (i as u16);
            if cx >= self.width {
                break;
            }
            self.set(cx, y, ch, style);
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, ch: char, style: Style) {
        for dy in 0..rect.height {
            for dx in 0..rect.width {
                self.set(rect.x + dx, rect.y + dy, ch, style);
            }
        }
    }

    pub fn draw_box(&mut self, rect: Rect, title: &str, focused: bool) {
        if rect.width < 2 || rect.height < 2 {
            return;
        }
        let border_style = if focused {
            Style::bold_fg(Color::Cyan)
        } else {
            Style::fg(Color::DarkGray)
        };

        self.set(rect.x, rect.y, '┌', border_style);
        self.set(rect.x + rect.width - 1, rect.y, '┐', border_style);
        self.set(rect.x, rect.y + rect.height - 1, '└', border_style);
        self.set(
            rect.x + rect.width - 1,
            rect.y + rect.height - 1,
            '┘',
            border_style,
        );

        for dx in 1..rect.width - 1 {
            self.set(rect.x + dx, rect.y, '─', border_style);
            self.set(rect.x + dx, rect.y + rect.height - 1, '─', border_style);
        }
        for dy in 1..rect.height - 1 {
            self.set(rect.x, rect.y + dy, '│', border_style);
            self.set(rect.x + rect.width - 1, rect.y + dy, '│', border_style);
        }

        if !title.is_empty() && rect.width > 4 {
            let formatted = format!(" {title} ");
            let title_style = if focused {
                Style::bold_fg(Color::Yellow)
            } else {
                Style::fg(Color::White)
            };
            self.draw_text(rect.x + 2, rect.y, rect.width.saturating_sub(4), &formatted, title_style);
        }
    }

    /// Renders the buffer as plain text (without ANSI escapes) for deterministic snapshot tests.
    pub fn to_plain_string(&self) -> String {
        let mut out = String::new();
        for y in 0..self.height {
            let row_start = (y as usize) * (self.width as usize);
            let row_end = row_start + (self.width as usize);
            let line: String = self.cells[row_start..row_end].iter().map(|c| c.ch).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// Renders the buffer into an ANSI escape sequence frame string for terminal output.
    pub fn to_ansi_frame(&self) -> String {
        let mut out = String::with_capacity((self.width as usize) * (self.height as usize) * 2);
        out.push_str("\x1b[H"); // Cursor to top-left (1, 1)

        for y in 0..self.height {
            if y > 0 {
                out.push_str("\r\n");
            }
            let mut cur_style = Style::default();
            out.push_str("\x1b[0m");

            for x in 0..self.width {
                let cell = self.cells[(y as usize) * (self.width as usize) + (x as usize)];
                if cell.style != cur_style {
                    out.push_str("\x1b[0m");
                    if cell.style.bold {
                        out.push_str("\x1b[1m");
                    }
                    out.push_str(cell.style.fg.fg_ansi());
                    out.push_str(cell.style.bg.bg_ansi());
                    cur_style = cell.style;
                }
                out.push(cell.ch);
            }
            out.push_str("\x1b[0m");
        }
        out
    }
}
