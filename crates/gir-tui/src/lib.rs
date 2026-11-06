pub mod app;
pub mod theme;
pub mod views;

use std::io::{self, Read, Write};
use std::mem::MaybeUninit;

use gir_core::{GirError, Repository};

pub use app::{ActivePane, AppState, InputMode, KeyEvent};
pub use theme::{Color, Rect, ScreenBuffer, Style};
pub use views::render_app;

/// Renders a single headless frame of `AppState` to a `ScreenBuffer` of `(width, height)`.
pub fn render_headless_frame(app: &AppState, width: u16, height: u16) -> ScreenBuffer {
    let mut buf = ScreenBuffer::new(width, height);
    render_app(&mut buf, app);
    buf
}

/// Launches the interactive full-screen `gir tui` terminal interface.
pub fn run_tui(repo: Repository) -> Result<(), GirError> {
    let mut app = AppState::new(repo)?;
    let _guard = RawTerminalGuard::enter()?;

    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let (width, height) = terminal_size();
        let buf = render_headless_frame(&app, width, height);
        write!(stdout, "{}", buf.to_ansi_frame())?;
        stdout.flush()?;

        let Some(key) = read_key_event(&mut stdin)? else {
            break;
        };
        if app.handle_key(key)? {
            break;
        }
    }

    Ok(())
}

struct RawTerminalGuard {
    orig_termios: Option<libc::termios>,
}

impl RawTerminalGuard {
    fn enter() -> io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        let orig_termios = unsafe {
            let mut t = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(fd, t.as_mut_ptr()) == 0 {
                let orig = t.assume_init();
                let mut raw = orig;
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(fd, libc::TCSANOW, &raw);
                Some(orig)
            } else {
                None
            }
        };

        let mut stdout = io::stdout();
        // Enter alternate screen buffer and hide cursor
        write!(stdout, "\x1b[?1049h\x1b[?25l\x1b[2J")?;
        stdout.flush()?;

        Ok(Self { orig_termios })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        if let Some(ref orig) = self.orig_termios {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, orig);
            }
        }
        let mut stdout = io::stdout();
        let _ = write!(stdout, "\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
    }
}

fn terminal_size() -> (u16, u16) {
    unsafe {
        let mut ws = MaybeUninit::<libc::winsize>::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) == 0 {
            let ws = ws.assume_init();
            if ws.ws_col >= 20 && ws.ws_row >= 6 {
                return (ws.ws_col, ws.ws_row);
            }
        }
    }
    (100, 30)
}

fn read_key_event<R: Read>(reader: &mut R) -> io::Result<Option<KeyEvent>> {
    let mut b = [0u8; 1];
    if reader.read(&mut b)? == 0 {
        return Ok(None);
    }

    match b[0] {
        b'\r' | b'\n' => Ok(Some(KeyEvent::Enter)),
        b'\t' => Ok(Some(KeyEvent::Tab)),
        0x7f | 0x08 => Ok(Some(KeyEvent::Backspace)),
        0x03 => Ok(Some(KeyEvent::Esc)), // Ctrl-C exits cleanly
        0x1b => {
            // Check for ANSI arrow key sequence `\x1b[A/B/C/D`
            let mut seq = [0u8; 2];
            if reader.read(&mut seq)? == 2 && seq[0] == b'[' {
                match seq[1] {
                    b'A' => return Ok(Some(KeyEvent::Up)),
                    b'B' => return Ok(Some(KeyEvent::Down)),
                    b'C' => return Ok(Some(KeyEvent::Right)),
                    b'D' => return Ok(Some(KeyEvent::Left)),
                    _ => {}
                }
            }
            Ok(Some(KeyEvent::Esc))
        }
        ch => Ok(Some(KeyEvent::Char(ch as char))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn tui_headless_render_and_key_navigation_stage_commit_and_undo() {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path(), false).unwrap();
        let mut cfg = repo.local_config().unwrap();
        cfg.set("user.name", "Irfan").unwrap();
        cfg.set("user.email", "irfan@example.com").unwrap();
        cfg.save_to(repo.git_dir().join("config")).unwrap();

        fs::write(dir.path().join("hello.rs"), "fn main() {}\n").unwrap();

        let mut app = AppState::new(repo).unwrap();
        assert_eq!(app.status_items.len(), 1);
        assert!(app.status_items[0].is_untracked);

        let frame1 = render_headless_frame(&app, 100, 28).to_plain_string();
        assert!(frame1.contains("gir tui"));
        assert!(frame1.contains("hello.rs"));

        // Press Space to stage `hello.rs`
        app.handle_key(KeyEvent::Char(' ')).unwrap();
        assert!(app.status_items[0].is_staged);

        // Press 'c' to open commit modal, type message, press Enter to commit
        app.handle_key(KeyEvent::Char('c')).unwrap();
        for ch in "feat: initial via tui".chars() {
            app.handle_key(KeyEvent::Char(ch)).unwrap();
        }
        app.handle_key(KeyEvent::Enter).unwrap();
        assert!(app.status_items.is_empty());
        assert_eq!(app.log_items.len(), 1);

        // Switch to [3] LogGraph pane and verify commit is rendered
        app.handle_key(KeyEvent::Char('3')).unwrap();
        let frame_log = render_headless_frame(&app, 100, 28).to_plain_string();
        assert!(frame_log.contains("feat: initial via tui"));

        // Modify `hello.rs`, switch back to [1] Status, Tab into Diff pane, and stage + unstage hunk via Space
        fs::write(dir.path().join("hello.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        app.handle_key(KeyEvent::Char('1')).unwrap();
        app.handle_key(KeyEvent::Char('r')).unwrap();
        assert_eq!(app.status_items.len(), 1);
        assert!(!app.status_items[0].is_staged);

        // Tab into Diff pane and press Space to stage the hunk
        app.handle_key(KeyEvent::Tab).unwrap();
        app.handle_key(KeyEvent::Char(' ')).unwrap();
        assert_eq!(app.status_items.len(), 1);
        assert!(app.status_items[0].is_staged);

        // Press Space again in Diff pane on the staged diff to unstage the hunk
        app.handle_key(KeyEvent::Char(' ')).unwrap();
        assert_eq!(app.status_items.len(), 1);
        assert!(!app.status_items[0].is_staged);
    }
}
