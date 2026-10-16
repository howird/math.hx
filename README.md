# math.hx

A [helix](https://github.com/mattwparas/helix/tree/steel-event-system) plugin, written with
steel, that shows rendered [typst](https://typst.app) math right in the editor, using the
kitty graphics protocol (kitty, ghostty).

```
Inline $frac(a, b) + sqrt(x)$ and the rest of the line.
        [image] (on blank lines added under it, while the cursor is in the math)

$$
sum_(i=1)^n alpha_i
$$
[image]
```

## Previews

Two settings decide when the rendered math shows and how it's drawn:

| auto-render | display | what you get |
|---|---|---|
| `'on` | `'virtual` (default) | while the cursor is in a math span, the image goes on blank lines reserved below it |
| `'on` | `'popup` | while the cursor is in a math span, the image is drawn over the text below it |
| `'off` | either | the same, but only after `:math-preview`, until the cursor leaves that span |

Either way, inline math is drawn on the row under the span's last line, starting at the
column right after the opening `$`; display math on the line after the closing delimiter,
at the opening delimiter's column.

With `'virtual`, the rows it needs are first added under that line as blank lines made with
inlay hints, so the buffer itself is never touched and no text is covered. The extra rows
have no line numbers.

With `'popup`, it covers the text there, and isn't moved to fit the view: whatever falls
outside it is cut off.

Either way it re-renders as you type; while the math doesn't compile, the last good image
stays and the error shows in the status line.

Auto-render runs in markdown buffers by default, where `$...$` and `$$...$$` are found with
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark), so code spans and fenced
code blocks are skipped. Typst buffers are left to tinymist and its PDF preview, but can be
turned on, see [Settings](#settings). `:math-preview` works in markdown and typst whatever
the settings.

Helix only tells plugins where the primary cursor is on screen, so the image is placed
relative to it, which only holds without soft-wrap. With soft-wrap on, the image is drawn
below the cursor instead, over the text, in both display modes. Only the focused view shows
it.

`]m` / `[m` select the next / previous math span, like helix's other `]` / `[` motions.

## LaTeX

By default the math in markdown is read as typst. It can be read as LaTeX instead, which is
translated to typst with [mitex](https://github.com/mitex-rs/mitex) and evaluated with the
typst side of mitex bundled in `assets/mitex` (see `assets/mitex/NOTICE`). `'auto` reads a
span as LaTeX when it contains a backslash command like `\frac`, and as typst otherwise; it
doesn't fall back to the other syntax when a span doesn't compile. Typst buffers are always
typst.

`latex->typst` converts a single LaTeX snippet to typst source, as mitex emits it: the result
calls into mitex's scope (`mitexsqrt`, `mitexarray`, ...), so evaluate it with that in scope, as
mitex's own `mimath` does (`eval(..., scope: mitex-scope)`).

## Settings

In your `init.scm`, after requiring the plugin:

```scheme
(math-auto-render-languages! '("markdown" "typst")) ; where auto-render runs, default '("markdown")
(math-auto-render! 'off)                             ; 'on (default) or 'off
(math-display! 'popup)                               ; 'virtual (default) or 'popup
(math-background! 'background)                       ; 'popup (ui.popup, default) or 'background (ui.background)
(math-syntax! 'auto)                                 ; 'typst (default), 'latex or 'auto
(math-font-size! 24)                                 ; pixels, inline and display; 'auto (default) matches the terminal
```

They also work as typed commands, e.g. `:math-display! popup`, `:math-font-size! 24`, `:math-syntax! latex`,
`:math-auto-render-languages! markdown typst`, and `:math-auto-render-toggle` switches
auto-render on and off.

## How images are drawn

Equations are compiled with the [typst](https://github.com/typst/typst) compiler linked into
the plugin's dylib, against a minimal world: one in-memory source plus mitex's definitions, no
packages, and only New Computer Modern (math and text) embedded from `assets/fonts` (see
`assets/fonts/NOTICE` for their licenses). They're rasterized with `typst-render`, padded to
whole terminal cells, and coloured with the theme's `ui.text`. A warm render takes well under
a millisecond.

Helix can't draw images, so the dylib writes the image straight to `/dev/tty` using the kitty
graphics protocol as a virtual placement, and the plugin only draws
[Unicode placeholder](https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders)
cells whose foreground colour encodes the image id. That needs a terminal supporting
placeholders (kitty, ghostty) and helix emitting truecolor.

Try the renderer outside helix with:

```sh
cargo run --release -- render 'integral_0^1 x^2 dif x' --fg '#000000' -o eq.png
cargo run --release -- kitty 'mat(1, 2; 3, 4)'   # shows it in the current terminal
cargo run --release -- kitty '\sqrt[3]{x}' --latex
```

## Install

With the `forge` command line tool (requires a rust toolchain), from the root of this repo:

`forge install`

Then in your `init.scm`:

```scheme
(require "math-hx/math.scm")
```

`]m` / `[m` are bound globally when the plugin loads.

## Notes

- `'virtual` reserves its space with inlay hints. A language server that sends its own inlay
  hints for the buffer (tinymist does, for typst) replaces them, and the image may cover text
  until the next command puts the space back. Clearing the preview clears the view's other
  inlay hints too, until the server sends them again.
- Inline diagnostics shown between the cursor's line and the math's last line, and inlay
  hints from a language server before the math on its line, shift the image by their size.

## Development

`nix run . -- file.md` starts steelix with stock settings and only this plugin. The config
comes from the store and `STEEL_HOME` is a temp dir removed on exit (steelix writes its
builtin modules there), so `~/.config/helix` and `~/.local/share/steel` are never read or
touched. `nix build .#math-hx`
builds just the plugin (the `native` output holds the dylib).

`cargo run` (no arguments) regenerates `math-hx.scm`, the FFI bindings for the dylib.

The typst compiler can't be trimmed down to just math layout (it needs eval, styles and
fonts like any content), and none of its crates have features to switch off, so the dylib is
about 40 MB stripped: typst-library alone is ~5 MB of code, plus citation (hayagriva), wasm
plugin, image decoding and ICU data that math never uses.
