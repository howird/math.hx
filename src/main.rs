use std::time::Instant;

use math_hx::{
    build_module, default_px_per_em, kitty, render_math, render_math_cells, MathStyle, Syntax,
};

const USAGE: &str = "\
usage: math-hx                       regenerate math-hx.scm
       math-hx render <math> [opts]  write the equation to a PNG
       math-hx kitty <math> [opts]   show the equation in this terminal

options: --inline       inline rather than display math
         --latex        read the math as LaTeX
         --fg <#hex>    text colour (default #ffffff)
         --em <px>      font size in pixels (default: match the terminal)
         -o <file>      output for `render` (default math.png)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => build_module()
            .emit_package_to_file("libmath_hx", "math-hx.scm")
            .map_err(|err| format!("{err:?}")),
        Some("render") => render(&args[1..]),
        Some("kitty") => show(&args[1..]),
        Some(_) => Err(USAGE.to_string()),
    };
    if let Err(err) = result {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

struct Args {
    body: String,
    style: MathStyle,
    out: String,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut body = None;
    let mut style = MathStyle {
        syntax: Syntax::Typst,
        block: true,
        fg: "#ffffff".into(),
        px_per_em: default_px_per_em(),
    };
    let mut out = "math.png".to_string();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().cloned().ok_or(USAGE.to_string());
        match arg.as_str() {
            "--inline" => style.block = false,
            "--latex" => style.syntax = Syntax::Latex,
            "--fg" => style.fg = value()?,
            "--em" => style.px_per_em = value()?.parse().map_err(|_| USAGE)?,
            "-o" => out = value()?,
            _ if body.is_none() => body = Some(arg.clone()),
            _ => return Err(USAGE.to_string()),
        }
    }
    let body = body.ok_or(USAGE)?;
    Ok(Args { body, style, out })
}

fn render(args: &[String]) -> Result<(), String> {
    let Args { body, style, out } = parse(args)?;
    let start = Instant::now();
    let pixmap = render_math(&body, &style)?;
    let cold = start.elapsed();
    let start = Instant::now();
    render_math(&body, &style)?;
    let warm = start.elapsed();
    pixmap.save_png(&out).map_err(|err| err.to_string())?;
    println!(
        "{out}: {}x{} px (cold {cold:.1?}, warm {warm:.1?})",
        pixmap.width(),
        pixmap.height()
    );
    Ok(())
}

fn show(args: &[String]) -> Result<(), String> {
    let Args { body, style, .. } = parse(args)?;
    let cell = kitty::cell_size();
    let rendered = render_math_cells(&body, &style, cell)?;
    let id = kitty::next_id();
    kitty::transmit(id, &rendered.png, rendered.cols, rendered.rows)
        .map_err(|err| err.to_string())?;
    let (r, g, b) = kitty::id_color(id);
    for row in 0..rendered.rows {
        let text = kitty::placeholder_row(id, row, rendered.cols);
        println!("\x1b[38;2;{r};{g};{b}m{text}\x1b[39m");
    }
    println!(
        "id {id}, {}x{} cells of {}x{} px",
        rendered.cols, rendered.rows, cell.0, cell.1
    );
    Ok(())
}
