use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, Once};

use abi_stable::std_types::{RString, RVec};
use pulldown_cmark::{Event, Options, Parser};
use steel::steel_vm::ffi::{FFIModule, FFIValue, RegisterFFIFn};
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{ast, FileId, LinkedNode, Source, SyntaxKind, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::{LazyHash, Scalar};
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub mod kitty;

steel::declare_module!(build_module);

fn math_module() -> FFIModule {
    let mut module = FFIModule::new("steel/math-hx");

    module
        .register_fn("latex->typst", latex_to_typst)
        .register_fn("markdown-math-spans", markdown_math_spans)
        .register_fn("markdown-math-recovers", markdown_math_recovers)
        .register_fn("typst-math-spans", typst_math_spans)
        .register_fn("typst-math-render", typst_math_render)
        .register_fn("typst-math-placeholder", typst_math_placeholder)
        .register_fn("typst-math-id-color", typst_math_id_color)
        .register_fn("typst-math-clear", typst_math_clear)
        .register_fn("typst-math-epoch", typst_math_epoch)
        .register_fn("display-width", display_width);

    module
}

/// A math span found in a document, in char offsets so it can be handed
/// straight to helix.
struct MathSpan {
    start: usize,
    end: usize,
    /// The math without its delimiters.
    body: String,
    /// Display math (`$$...$$` in markdown, `$ ... $` in typst).
    block: bool,
}

impl MathSpan {
    /// `[start end body block]`, as handed to scheme.
    fn into_ffi(self) -> FFIValue {
        let span: RVec<FFIValue> = [
            FFIValue::IntV(self.start as isize),
            FFIValue::IntV(self.end as isize),
            FFIValue::StringV(RString::from(self.body)),
            FFIValue::BoolV(self.block),
        ]
        .into_iter()
        .collect();
        FFIValue::Vector(span)
    }
}

/// Byte -> char offset conversion for one text, advanced monotonically since
/// spans are found in document order.
fn char_offsets(text: &str) -> impl FnMut(usize) -> usize + '_ {
    let mut byte_cursor = 0;
    let mut char_cursor = 0;
    move |byte: usize| {
        if byte >= byte_cursor {
            char_cursor += text[byte_cursor..byte].chars().count();
        } else {
            char_cursor -= text[byte..byte_cursor].chars().count();
        }
        byte_cursor = byte;
        char_cursor
    }
}

/// `$...$` / `$$...$$` math in markdown.
fn find_math_spans(text: &str) -> Vec<MathSpan> {
    let mut spans = Vec::new();
    let mut to_char = char_offsets(text);

    for (event, range) in Parser::new_ext(text, Options::ENABLE_MATH).into_offset_iter() {
        let (body, block) = match event {
            Event::InlineMath(body) => (body, false),
            Event::DisplayMath(body) => (body, true),
            _ => continue,
        };
        let start = to_char(range.start);
        let end = to_char(range.end);
        spans.push(MathSpan {
            start,
            end,
            body: body.into_string(),
            block,
        });
    }

    spans
}

/// Whether `text[start..end]` (char offsets, delimiters included), which
/// markdown doesn't read as math, would be math again once the states that
/// typing passes through are tidied up: whitespace just inside `$...$`, or
/// blank lines inside `$$...$$`. Anything else that stops it being math, like
/// an escaped delimiter or a code span, still does.
fn math_recovers(text: &str, start: usize, end: usize, block: bool) -> bool {
    if start > end {
        return false;
    }
    let mut bytes = text
        .char_indices()
        .map(|(b, _)| b)
        .chain([text.len()])
        .skip(start);
    let Some(from) = bytes.next() else {
        return false;
    };
    let to = if end == start {
        from
    } else {
        bytes.nth(end - start - 1).unwrap_or(text.len())
    };
    let delim = if block { "$$" } else { "$" };
    let Some(body) = text[from..to]
        .strip_prefix(delim)
        .and_then(|s| s.strip_suffix(delim))
    else {
        return false;
    };
    let tidied = if block {
        let lines: Vec<_> = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return false;
        }
        let open = if body.starts_with('\n') { "\n" } else { "" };
        let close = if body.ends_with('\n') { "\n" } else { "" };
        format!("$${open}{}{close}$$", lines.join("\n"))
    } else {
        let body = body.trim();
        if body.is_empty() {
            return false;
        }
        format!("${body}$")
    };
    let spliced = format!("{}{tidied}{}", &text[..from], &text[to..]);
    find_math_spans(&spliced)
        .iter()
        .any(|span| span.start == start && span.block == block)
}

/// Equations in a typst document. The parser still makes an equation of an
/// unclosed `$` (its opening `$` turned into an error), so only those with a
/// `$` at both ends count, as markdown only counts closed spans.
fn find_typst_math_spans(text: &str) -> Vec<MathSpan> {
    type Found = (std::ops::Range<usize>, std::ops::Range<usize>, bool);
    fn visit(node: LinkedNode, out: &mut Vec<Found>) {
        if let Some(eq) = node.cast::<ast::Equation>() {
            let kind = |child: Option<LinkedNode>| child.map(|c| c.kind());
            let closed = kind(node.children().next()) == Some(SyntaxKind::Dollar)
                && kind(node.children().next_back()) == Some(SyntaxKind::Dollar);
            let body = node.children().find(|c| c.kind() == SyntaxKind::Math);
            if let (true, Some(body)) = (closed, body) {
                out.push((node.range(), body.range(), eq.block()));
            }
            return;
        }
        for child in node.children() {
            visit(child, out);
        }
    }

    let root = typst::syntax::parse(text);
    let mut found = Vec::new();
    visit(LinkedNode::new(&root), &mut found);

    let mut to_char = char_offsets(text);
    found
        .into_iter()
        .map(|(range, body, block)| MathSpan {
            start: to_char(range.start),
            end: to_char(range.end),
            body: text[body].to_string(),
            block,
        })
        .collect()
}

/// How a math body is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Syntax {
    Typst,
    Latex,
}

/// A backslash command like `\frac` or `\alpha`, which never means anything
/// useful in typst math.
fn looks_like_latex(body: &str) -> bool {
    body.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'\\' && w[1].is_ascii_alphabetic())
}

/// The syntax to read `body` as, for the `syntax` setting `"typst"`,
/// `"latex"` or `"auto"`. `auto` never falls back to the other syntax when
/// the detected one fails: a half-typed body then reports its error rather
/// than rendering as nonsense in the other syntax.
fn syntax_for(body: &str, syntax: &str) -> Result<Syntax, String> {
    match syntax {
        "typst" => Ok(Syntax::Typst),
        "latex" => Ok(Syntax::Latex),
        "auto" if looks_like_latex(body) => Ok(Syntax::Latex),
        "auto" => Ok(Syntax::Typst),
        _ => Err(format!(
            "unknown math syntax `{syntax}`, expected typst, latex or auto"
        )),
    }
}

/// mitex's translation of `latex`, as upstream emits it: it calls into
/// `mitex-scope` (`mitexsqrt`, `mitexarray`, ...), so it only evaluates with
/// that in scope.
fn convert(latex: &str) -> Result<String, String> {
    mitex::convert_math(latex, None).map_err(|err| err.trim().to_string())
}

/// Convert a single LaTeX math snippet (without delimiters) to typst math.
/// Returns the error message as a string prefixed with "error: " on failure.
fn latex_to_typst(latex: String) -> String {
    convert(&latex).unwrap_or_else(|err| format!("error: {err}"))
}

/// `[start end body block]` for each math span in markdown `text`, with the
/// body as written (see [`Syntax`] for how it is read).
fn markdown_math_spans(text: String) -> FFIValue {
    FFIValue::Vector(
        find_math_spans(&text)
            .into_iter()
            .map(MathSpan::into_ffi)
            .collect(),
    )
}

/// See [`math_recovers`].
fn markdown_math_recovers(text: String, start: usize, end: usize, block: bool) -> bool {
    math_recovers(&text, start, end, block)
}

/// `[start end body block]` for each equation in typst `text`.
fn typst_math_spans(text: String) -> FFIValue {
    FFIValue::Vector(
        find_typst_math_spans(&text)
            .into_iter()
            .map(MathSpan::into_ffi)
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Rendering typst math to images.
//
// The typst compiler can't be cut down to just its math layout, which needs
// eval, styles and fonts like any other content. What we leave out is
// everything around it: the only files are an in-memory source and the typst
// side of mitex (for LaTeX), there are no packages, and the fonts are just
// New Computer Modern (math + text) instead of the 10 MB set in typst-assets.

/// The font size equations are typeset at. Only matters relative to the
/// pixel density, see [`MathStyle::px_per_em`].
const FONT_PT: f64 = 10.0;

static FONTS: LazyLock<(LazyHash<FontBook>, Vec<Font>)> = LazyLock::new(|| {
    let fonts: Vec<Font> = [
        include_bytes!("../assets/fonts/NewCMMath-Regular.otf").as_slice(),
        include_bytes!("../assets/fonts/NewCM10-Regular.otf").as_slice(),
    ]
    .into_iter()
    .flat_map(|data| Font::iter(Bytes::new(data)))
    .collect();
    (LazyHash::new(FontBook::from_fonts(&fonts)), fonts)
});

static LIBRARY: LazyLock<LazyHash<Library>> =
    LazyLock::new(|| LazyHash::new(<Library as LibraryExt>::default()));

/// mitex's typst definitions, which its LaTeX translation calls into, by
/// path. `@preview/xarrow` (imported by `standard.typ`) is served from a
/// stand-in rather than the real package.
const MITEX_FILES: [(Option<&str>, &str, &str); 4] = [
    (
        None,
        "/mitex/specs/mod.typ",
        include_str!("../assets/mitex/specs/mod.typ"),
    ),
    (
        None,
        "/mitex/specs/prelude.typ",
        include_str!("../assets/mitex/specs/prelude.typ"),
    ),
    (
        None,
        "/mitex/specs/latex/standard.typ",
        include_str!("../assets/mitex/specs/latex/standard.typ"),
    ),
    (
        Some("xarrow"),
        "/lib.typ",
        include_str!("../assets/mitex/xarrow.typ"),
    ),
];

const XARROW_MANIFEST: &str =
    "[package]\nname = \"xarrow\"\nversion = \"0.2.0\"\nentrypoint = \"lib.typ\"\n";

/// Parsed [`MITEX_FILES`], so they're only parsed once.
static MITEX_SOURCES: LazyLock<Mutex<HashMap<FileId, Source>>> = LazyLock::new(Default::default);

fn package_name(id: FileId) -> Option<&'static str> {
    match id.get().root() {
        VirtualRoot::Package(spec) => Some(spec.name.as_str()),
        VirtualRoot::Project => None,
    }
}

fn mitex_source(id: FileId) -> FileResult<Source> {
    let path = id.get();
    let package = package_name(id);
    let vpath = path.vpath().get_with_slash();
    let (_, _, text) = MITEX_FILES
        .iter()
        .find(|(p, v, _)| *p == package && *v == vpath)
        .ok_or(FileError::AccessDenied)?;
    let mut sources = MITEX_SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    Ok(sources
        .entry(id)
        .or_insert_with(|| Source::new(id, text.to_string()))
        .clone())
}

/// A typst world holding the equation's source and mitex's definitions.
/// Compiling a fresh one per equation is cheap: the library and fonts are
/// shared, and typst memoizes layout across compiles.
struct MathWorld(Source);

impl World for MathWorld {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &FONTS.0
    }

    fn main(&self) -> FileId {
        self.0.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.0.id() {
            Ok(self.0.clone())
        } else {
            mitex_source(id)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if package_name(id) == Some("xarrow") && id.get().vpath().get_with_slash() == "/typst.toml"
        {
            Ok(Bytes::new(XARROW_MANIFEST.as_bytes()))
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.1.get(index).cloned()
    }

    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}

/// How to typeset an equation.
#[derive(Clone, Debug, Hash)]
pub struct MathStyle {
    /// How the body is written.
    pub syntax: Syntax,
    /// Display rather than inline math.
    pub block: bool,
    /// Text colour, `#rgb` or `#rrggbb`.
    pub fg: String,
    /// Pixels per em, i.e. the rendered font size in pixels.
    pub px_per_em: u32,
}

/// The font size that matches the terminal's text, in pixels per em.
pub fn default_px_per_em() -> u32 {
    kitty::cell_size().1 * 17 / 20
}

/// An equation rendered to a PNG covering exactly `cols` x `rows` cells.
pub struct Rendered {
    pub png: Vec<u8>,
    pub cols: u32,
    pub rows: u32,
}

fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

fn math_source(body: &str, style: &MathStyle) -> Result<String, String> {
    if !is_hex_color(&style.fg) {
        return Err(format!("not a hex colour: {}", style.fg));
    }
    let body = body.trim();
    let equation = match style.syntax {
        Syntax::Typst if style.block => format!("$ {body} $"),
        Syntax::Typst => format!("${body}$"),
        // As mitex's own `mimath` does it: evaluate the translation with the
        // definitions it calls into in scope.
        Syntax::Latex => {
            let typst = convert(body)?;
            format!(
                "#import \"/mitex/specs/mod.typ\": mitex-scope\n\
                 #math.equation(block: {block}, eval({code:?}, scope: mitex-scope))",
                block = style.block,
                code = format!("${typst}$"),
            )
        }
    };
    // Lines are normally only as tall as cap height to baseline, which would
    // crop inline math to that on the auto-sized page. Size them from the
    // ascender, so the baseline sits at the same depth below the top in every
    // equation that doesn't rise above it, down to the glyphs' bounds.
    Ok(format!(
        "#set page(width: auto, height: auto, margin: 1pt, fill: none)\n\
         #set text(font: \"New Computer Modern\", size: {FONT_PT}pt, fill: rgb(\"{fg}\"),\n\
                   top-edge: \"ascender\", bottom-edge: \"bounds\")\n\
         #show math.equation: set text(font: \"New Computer Modern Math\")\n\
         {equation}\n",
        fg = style.fg,
    ))
}

/// Typeset `body` (typst math, without `$`) and rasterize it, cropped to the
/// equation.
pub fn render_math(body: &str, style: &MathStyle) -> Result<Pixmap, String> {
    let world = MathWorld(Source::detached(math_source(body, style)?));
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|errors| {
            errors
                .iter()
                .map(|err| err.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
    let page = doc.pages().first().ok_or("equation produced no output")?;
    let options = typst_render::RenderOptions {
        pixel_per_pt: Scalar::new(f64::from(style.px_per_em) / FONT_PT),
        render_bleed: false,
    };
    Ok(typst_render::render(page, &options))
}

/// Render `body` padded to a whole number of `cell`-sized terminal cells, so
/// the terminal shows it at its native size. The equation is centred
/// horizontally; inline math sits at the top, so it lines up with the text row
/// it starts on and any extra rows hang below, and display math is centred
/// vertically.
pub fn render_math_cells(
    body: &str,
    style: &MathStyle,
    (cell_w, cell_h): (u32, u32),
) -> Result<Rendered, String> {
    let pixmap = render_math(body, style)?;
    let cols = pixmap.width().div_ceil(cell_w).max(1);
    let rows = pixmap.height().div_ceil(cell_h).max(1);
    if rows > kitty::MAX_ROWS {
        return Err(format!(
            "equation is {rows} rows tall, at most {} fit",
            kitty::MAX_ROWS
        ));
    }
    let mut padded = Pixmap::new(cols * cell_w, rows * cell_h).ok_or("empty image")?;
    let left = (padded.width() - pixmap.width()) / 2;
    let top = if style.block {
        (padded.height() - pixmap.height()) / 2
    } else {
        0
    };
    padded.draw_pixmap(
        left as i32,
        top as i32,
        pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    let png = padded.encode_png().map_err(|err| err.to_string())?;
    Ok(Rendered { png, cols, rows })
}

/// A rendered equation and the terminal image id it is shown under.
struct Shown {
    id: u32,
    rendered: Rendered,
}

/// Rendered equations by (body, style, cell size), so each is only typeset
/// once and keeps one image id.
static SHOWN: LazyLock<Mutex<HashMap<u64, Shown>>> = LazyLock::new(Default::default);

/// Past this many equations, they are all dropped and re-rendered on demand.
const MAX_SHOWN: usize = 512;

/// Bumped whenever [`clear_shown`] deletes images, so callers holding an
/// image id know to render again.
static EPOCH: AtomicUsize = AtomicUsize::new(0);

/// Forget every rendered equation and delete its image from the terminal.
fn clear_shown(shown: &mut HashMap<u64, Shown>) {
    EPOCH.fetch_add(1, Ordering::Relaxed);
    for (_, image) in shown.drain() {
        let _ = kitty::delete(image.id);
    }
}

/// The text colour and cell size of the last render. When either changes (a
/// theme change, a font resize), every image shown so far is stale.
static LAST_LOOK: Mutex<Option<(String, (u32, u32))>> = Mutex::new(None);

/// Delete the images and temporary files left over when helix exits.
extern "C" fn clean_up_at_exit() {
    if let Ok(mut shown) = SHOWN.try_lock() {
        clear_shown(&mut shown);
    }
    kitty::remove_temp_files();
}

/// Render `body` (or reuse an earlier render) and upload it to the terminal,
/// returning `(id, cols, rows)`. The upload is repeated every time: with
/// `q=2` we can't tell whether an earlier one reached the terminal, and a
/// lost one would otherwise leave a blank image for the rest of the session.
fn show(body: &str, style: &MathStyle) -> Result<(u32, u32, u32), String> {
    static AT_EXIT: Once = Once::new();
    // SAFETY: the handler is a plain function, and in a shared library glibc
    // runs it when the library is unloaded if that comes first.
    AT_EXIT.call_once(|| unsafe {
        libc::atexit(clean_up_at_exit);
    });

    let cell = kitty::cell_size();
    let mut hasher = DefaultHasher::new();
    (body.trim(), style, cell).hash(&mut hasher);
    let key = hasher.finish();

    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if !shown.contains_key(&key) {
        // Render first: if it fails, the images already shown must stay.
        let rendered = render_math_cells(body, style, cell)?;
        let mut last = LAST_LOOK.lock().unwrap_or_else(|e| e.into_inner());
        let look = (style.fg.clone(), cell);
        if shown.len() >= MAX_SHOWN || last.as_ref().is_some_and(|last| *last != look) {
            clear_shown(&mut shown);
        }
        *last = Some(look);
        shown.insert(
            key,
            Shown {
                id: kitty::next_id(),
                rendered,
            },
        );
    }

    let Shown { id, rendered } = &shown[&key];
    kitty::transmit(*id, &rendered.png, rendered.cols, rendered.rows)
        .map_err(|err| format!("writing to the terminal: {err}"))?;
    Ok((*id, rendered.cols, rendered.rows))
}

/// Render math `body` and upload it to the terminal, returning
/// `[id cols rows]`, or a string starting with "error: ". `syntax` is
/// `"typst"`, `"latex"` or `"auto"`. `px-per-em` 0 matches the terminal's
/// text size.
fn typst_math_render(
    body: String,
    block: bool,
    fg: String,
    px_per_em: isize,
    syntax: String,
) -> FFIValue {
    let px_per_em = match u32::try_from(px_per_em) {
        Ok(0) | Err(_) => default_px_per_em(),
        Ok(px) => px,
    };
    let shown = syntax_for(&body, &syntax).and_then(|syntax| {
        show(
            &body,
            &MathStyle {
                syntax,
                block,
                fg,
                px_per_em,
            },
        )
    });
    match shown {
        Ok((id, cols, rows)) => {
            let info: RVec<FFIValue> = [id, cols, rows]
                .into_iter()
                .map(|n| FFIValue::IntV(n as isize))
                .collect();
            FFIValue::Vector(info)
        }
        Err(err) => FFIValue::StringV(RString::from(format!("error: {err}"))),
    }
}

/// Text for `row` of image `id`, `cols` cells of it from the left; draw it
/// with the colour from `typst-math-id-color`.
fn typst_math_placeholder(id: usize, row: usize, cols: usize) -> String {
    kitty::placeholder_row(id as u32, row as u32, cols as u32)
}

/// How many cells `text` takes up on screen, with tabs every `tab-width`
/// cells, as helix lays out a line from its start.
fn display_width(text: String, tab_width: usize) -> usize {
    text_width(&text, tab_width)
}

fn text_width(text: &str, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    text.graphemes(true).fold(0, |width, grapheme| {
        if grapheme == "\t" {
            width + tab_width - width % tab_width
        } else {
            width + UnicodeWidthStr::width(grapheme).max(1)
        }
    })
}

/// `[r g b]`: the foreground colour that selects image `id` (its low 24 bits).
fn typst_math_id_color(id: usize) -> FFIValue {
    let (r, g, b) = kitty::id_color(id as u32);
    FFIValue::Vector(
        [r, g, b]
            .into_iter()
            .map(|c| FFIValue::IntV(c as isize))
            .collect(),
    )
}

/// Delete every uploaded image from the terminal, e.g. to free its memory.
fn typst_math_clear() {
    clear_shown(&mut SHOWN.lock().unwrap_or_else(|e| e.into_inner()));
}

/// Changes whenever uploaded images are deleted (by `typst-math-clear`, a
/// theme or font change, or the cache filling up); an id from
/// `typst-math-render` under an older epoch may be gone.
fn typst_math_epoch() -> usize {
    EPOCH.load(Ordering::Relaxed)
}

pub fn build_module() -> FFIModule {
    math_module()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_inline_and_display_math() {
        let text = "é $a^2$ and\n\n$$\n\\frac{1}{2}\n$$\n";
        let spans = find_math_spans(text);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].body, "a^2");
        assert!(!spans[0].block && spans[1].block);
        let chars: Vec<char> = text.chars().collect();
        let s0: String = chars[spans[0].start..spans[0].end].iter().collect();
        assert_eq!(s0, "$a^2$");
        let s1: String = chars[spans[1].start..spans[1].end].iter().collect();
        assert_eq!(s1, "$$\n\\frac{1}{2}\n$$");
    }

    #[test]
    fn skips_code() {
        let text = "`$x$`\n\n```\n$y$\n```\n";
        assert!(find_math_spans(text).is_empty());
    }

    #[test]
    fn recovers_math_while_typing() {
        assert!(math_recovers("é $x $ b", 2, 6, false));
        let block = "$$\nx\n\ny\n$$";
        assert!(math_recovers(block, 0, block.chars().count(), true));
        assert!(!math_recovers("a $x\\$ b", 2, 6, false));
        assert!(!math_recovers("`a $x` b$ c`", 3, 9, false));
        assert!(!math_recovers("a $ $ b", 2, 5, false));
        assert!(!math_recovers("a $x$ b", 2, 9, false));
        assert!(!math_recovers("a $x$ b", 2, 2, false));
    }

    #[test]
    fn measures_display_width() {
        assert_eq!(text_width("ab", 4), 2);
        assert_eq!(text_width("\tx", 4), 5);
        assert_eq!(text_width("a\tx", 4), 5);
        assert_eq!(text_width("abcd\t", 4), 8);
        assert_eq!(text_width("中文 a", 4), 6);
        assert_eq!(text_width("é", 4), 1);
    }

    #[test]
    fn finds_typst_equations() {
        let text = "= é\nSee $a^2$ and\n$ frac(1, 2) $\n`$no$` #let x = $y$\n";
        let spans = find_typst_math_spans(text);
        let bodies: Vec<_> = spans.iter().map(|s| (s.body.as_str(), s.block)).collect();
        assert_eq!(bodies, [("a^2", false), ("frac(1, 2)", true), ("y", false)]);
        let chars: Vec<char> = text.chars().collect();
        let s0: String = chars[spans[0].start..spans[0].end].iter().collect();
        assert_eq!(s0, "$a^2$");
    }

    #[test]
    fn skips_unclosed_typst_equations() {
        for text in ["a $", "$é", "$", "$ é", "#let y = $z"] {
            assert!(find_typst_math_spans(text).is_empty(), "{text:?}");
        }
        let spans = find_typst_math_spans("$ é $ $é");
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].body.as_str(), spans[0].block), ("é", true));
    }

    fn style(block: bool) -> MathStyle {
        MathStyle {
            syntax: Syntax::Typst,
            block,
            fg: "#ffffff".into(),
            px_per_em: 20,
        }
    }

    #[test]
    fn renders_math() {
        let inline = render_math("frac(a, b) + sum_(i=0)^n x_i", &style(false)).unwrap();
        let block = render_math("frac(a, b) + sum_(i=0)^n x_i", &style(true)).unwrap();
        assert!(inline.width() > inline.height());
        assert!(inline.data().iter().any(|&b| b != 0), "image is blank");
        // Display style sets the sum's limits above and below.
        assert!(block.height() > inline.height());
    }

    #[test]
    fn inline_math_is_not_cropped() {
        for body in ["frac(a, b)", "x^2 + y^2 = r^2", "g_j"] {
            let pixmap = render_math(body, &style(false)).unwrap();
            let row_ink = |y: u32| {
                (0..pixmap.width()).any(|x| pixmap.pixel(x, y).is_some_and(|p| p.alpha() > 0))
            };
            assert!(
                !row_ink(0) && !row_ink(pixmap.height() - 1),
                "{body} is cropped"
            );
        }
    }

    /// Rows of `pixmap` with any ink, as (first, last).
    fn ink_rows(pixmap: &Pixmap) -> (u32, u32) {
        let inked: Vec<u32> = (0..pixmap.height())
            .filter(|&y| {
                (0..pixmap.width()).any(|x| pixmap.pixel(x, y).is_some_and(|p| p.alpha() > 0))
            })
            .collect();
        (inked[0], inked[inked.len() - 1])
    }

    #[test]
    fn inline_baseline_is_consistent() {
        // Both sit on the baseline, so their ink ends on the same row.
        let low = render_math("a", &style(false)).unwrap();
        let tall = render_math("A", &style(false)).unwrap();
        assert_eq!(ink_rows(&low).1, ink_rows(&tall).1);
    }

    #[test]
    fn places_math_in_cells() {
        let r = render_math_cells("a + b", &style(false), (7, 40)).unwrap();
        let padded = Pixmap::decode_png(&r.png).unwrap();
        let alone = render_math("a + b", &style(false)).unwrap();
        // Top-aligned: ink starts where it does without padding.
        assert_eq!(ink_rows(&padded).0, ink_rows(&alone).0);
        let inked = |x: u32| {
            (0..padded.height()).any(|y| padded.pixel(x, y).is_some_and(|p| p.alpha() > 0))
        };
        let left = (0..padded.width()).find(|&x| inked(x)).unwrap();
        let right = padded.width() - 1 - (0..padded.width()).rev().find(|&x| inked(x)).unwrap();
        let alone_inked =
            |x: u32| (0..alone.height()).any(|y| alone.pixel(x, y).is_some_and(|p| p.alpha() > 0));
        let alone_left = (0..alone.width()).find(|&x| alone_inked(x)).unwrap();
        let alone_right =
            alone.width() - 1 - (0..alone.width()).rev().find(|&x| alone_inked(x)).unwrap();
        // Centred: the padding added on each side differs by at most a pixel.
        assert!(((left - alone_left) as i64 - (right - alone_right) as i64).abs() <= 1);
    }

    #[test]
    fn pads_to_cells() {
        let r = render_math_cells("mat(1, 2; 3, 4)", &style(true), (10, 20)).unwrap();
        assert!(r.rows >= 2);
        assert!(r.png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn reports_errors() {
        let err = render_math("frac(a", &style(false)).unwrap_err();
        assert!(err.contains("unclosed"), "{err}");
        assert!(render_math(
            "x",
            &MathStyle {
                fg: "red\")".into(),
                ..style(false)
            }
        )
        .is_err());
    }

    #[test]
    fn placeholders() {
        let row = kitty::placeholder_row(0x04_01_02_03, 1, 3);
        assert_eq!(row.chars().count(), 6);
        assert_eq!(kitty::id_color(0x04_01_02_03), (1, 2, 3));
        assert_ne!(kitty::next_id() & 0xFF_FFFF, 0);
    }

    #[test]
    fn detects_latex() {
        assert!(looks_like_latex(r"\frac{a}{b}"));
        assert!(!looks_like_latex("frac(a, b)"));
        assert!(!looks_like_latex(r"a \ b"));
        assert_eq!(syntax_for(r"\alpha", "auto").unwrap(), Syntax::Latex);
        assert_eq!(syntax_for("frac(a", "auto").unwrap(), Syntax::Typst);
        assert!(syntax_for("x", "tex").is_err());
    }

    #[test]
    fn renders_latex() {
        let latex = MathStyle {
            syntax: Syntax::Latex,
            ..style(true)
        };
        for body in [
            r"\frac{a}{b} + \sqrt[3]{x}",
            r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}",
            r"\begin{aligned} a &= b \\ c &= d \end{aligned}",
            r"\text{if } x \xrightarrow{f} \operatorname{tr}(A)",
        ] {
            render_math(body, &latex).unwrap_or_else(|err| panic!("{body}: {err}"));
        }
        assert!(render_math(r"\frac{a", &latex).is_err());
    }

    #[test]
    fn converts() {
        assert!(convert(r"\sqrt[3]{x}").unwrap().contains("mitexsqrt"));
    }
}
