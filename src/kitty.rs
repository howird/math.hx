//! Just enough of the kitty graphics protocol to show an image inside helix,
//! which has no image support of its own.
//!
//! The image data is written straight to the controlling terminal as a
//! *virtual* placement (`U=1`), which draws nothing by itself. It shows up
//! wherever cells contain the Unicode placeholder `U+10EEEE` with the image id
//! as their foreground colour, so helix only ever draws ordinary styled text.
//! Supported by kitty and ghostty.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column numbers and the image id's high byte are encoded as combining
/// marks after the placeholder; these are the first 256 entries of kitty's
/// `rowcolumn-diacritics.txt`.
const DIACRITICS: [char; 256] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
];

/// Tallest image, in rows, that placeholders can address. More diacritics
/// would allow taller ones, but nothing that tall fits on screen anyway.
pub const MAX_ROWS: u32 = 64;

/// Used when the terminal doesn't report its pixel size.
const FALLBACK_CELL: (u32, u32) = (10, 20);

static TTY: Mutex<Option<File>> = Mutex::new(None);

fn with_tty<T>(f: impl FnOnce(&mut File) -> io::Result<T>) -> io::Result<T> {
    let mut tty = TTY.lock().unwrap_or_else(|e| e.into_inner());
    if tty.is_none() {
        *tty = Some(OpenOptions::new().read(true).write(true).open("/dev/tty")?);
    }
    f(tty.as_mut().unwrap())
}

/// A fresh image id. The low 24 bits are the placeholders' foreground colour
/// and the high byte is their third diacritic. The top 16 bits are fixed per
/// process, from its pid and start time, so that two editors in one terminal
/// are unlikely to overwrite each other's images; the low 16 count up from 1,
/// keeping the colour non-zero.
pub fn next_id() -> u32 {
    static PREFIX: LazyLock<u32> = LazyLock::new(|| {
        let mut hasher = DefaultHasher::new();
        (std::process::id(), SystemTime::now()).hash(&mut hasher);
        hasher.finish() as u32 & 0xFFFF_0000
    });
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let low = COUNTER.fetch_add(1, Ordering::Relaxed) % 0xFFFF + 1;
    *PREFIX | low
}

/// Size of one terminal cell in pixels, as `(width, height)`.
pub fn cell_size() -> (u32, u32) {
    let size = with_tty(|tty| {
        use std::os::fd::AsRawFd;
        // SAFETY: TIOCGWINSZ only writes a `winsize` into the pointer given.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ws)
    });
    size.map_or(FALLBACK_CELL, |ws| {
        cell_from_winsize(ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel)
    })
}

/// Cell size from a terminal's size in cells and in pixels, or
/// [`FALLBACK_CELL`] when that doesn't give a cell at least a pixel each way.
fn cell_from_winsize(cols: u16, rows: u16, x_px: u16, y_px: u16) -> (u32, u32) {
    if cols == 0 || rows == 0 {
        return FALLBACK_CELL;
    }
    match (
        u32::from(x_px) / u32::from(cols),
        u32::from(y_px) / u32::from(rows),
    ) {
        (w, h) if w > 0 && h > 0 => (w, h),
        _ => FALLBACK_CELL,
    }
}

/// How long the terminal gets to read and delete a temporary file before we
/// delete it ourselves, for terminals that never read them.
const TEMP_FILE_LIFETIME: Duration = Duration::from_secs(5);

/// Temporary files written by [`transmit`], oldest first.
static WRITTEN: Mutex<VecDeque<(Instant, PathBuf)>> = Mutex::new(VecDeque::new());

/// Delete every temporary file still around, e.g. on exit. Skipped if another
/// thread is writing one.
pub fn remove_temp_files() {
    if let Ok(mut written) = WRITTEN.try_lock() {
        for (_, path) in written.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Write `png` to a new temporary file only we can read, deleting earlier
/// ones that have outlived [`TEMP_FILE_LIFETIME`].
fn write_temp_file(png: &[u8]) -> io::Result<PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = Instant::now();
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    while let Some((at, path)) = written.front() {
        if now.duration_since(*at) < TEMP_FILE_LIFETIME {
            break;
        }
        let _ = std::fs::remove_file(path);
        written.pop_front();
    }

    // Both kitty and ghostty only accept temporary files in the temp dir
    // whose name contains "tty-graphics-protocol". Each write gets its own
    // name, since an image is uploaded again before the terminal may have
    // deleted the last file, and `create_new` refuses anything already there
    // (a symlink included) in the shared temp dir.
    let path = std::env::temp_dir().join(format!(
        "math-hx-tty-graphics-protocol-{}-{}.png",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?
        .write_all(png)?;
    written.push_back((now, path.clone()));
    Ok(path)
}

/// Upload `png` as image `id`, to be shown over `cols` x `rows` placeholder
/// cells. Replaces any earlier image with the same id.
///
/// The PNG goes through a temporary file (`t=t`), which the terminal reads
/// and deletes, rather than inline: that's a single short escape instead of
/// 4 KiB chunks. If the first chunk is rejected, the remaining chunks are
/// read as new, broken images, and their error replies (which don't inherit
/// `q=2`) end up typed into the buffer. It does mean the terminal has to run
/// on the same machine.
pub fn transmit(id: u32, png: &[u8], cols: u32, rows: u32) -> io::Result<()> {
    let path = write_temp_file(png)?;
    let path = STANDARD.encode(path.as_os_str().as_encoded_bytes());
    with_tty(|tty| {
        write!(
            tty,
            "\x1b_Ga=T,f=100,t=t,U=1,i={id},c={cols},r={rows},q=2;{path}\x1b\\"
        )?;
        tty.flush()
    })
}

/// Free image `id` in the terminal.
pub fn delete(id: u32) -> io::Result<()> {
    with_tty(|tty| {
        write!(tty, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
        tty.flush()
    })
}

/// The text for row `row` of image `id`, `cols` cells of it from the left.
/// Only the first cell spells out its row, column and the id's high byte; the
/// terminal continues the rest of the row from it. Must be drawn with
/// [`id_color`] as its foreground colour.
pub fn placeholder_row(id: u32, row: u32, cols: u32) -> String {
    let mut s = String::with_capacity(4 * cols as usize + 6);
    if cols == 0 || row >= MAX_ROWS {
        return s;
    }
    s.push(PLACEHOLDER);
    s.push(DIACRITICS[row as usize]);
    s.push(DIACRITICS[0]);
    s.push(DIACRITICS[(id >> 24) as usize]);
    for _ in 1..cols {
        s.push(PLACEHOLDER);
    }
    s
}

/// The foreground colour carrying image `id`'s low 24 bits, as `(r, g, b)`.
pub fn id_color(id: u32) -> (u8, u8, u8) {
    ((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_size_never_zero() {
        assert_eq!(cell_from_winsize(80, 24, 800, 480), (10, 20));
        assert_eq!(cell_from_winsize(80, 24, 1, 480), FALLBACK_CELL);
        assert_eq!(cell_from_winsize(80, 24, 800, 1), FALLBACK_CELL);
        assert_eq!(cell_from_winsize(0, 24, 800, 480), FALLBACK_CELL);
    }
}
