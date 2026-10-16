// Stand-in for `@preview/xarrow:0.2.0`, which mitex imports for extensible
// arrows like `\xrightarrow{f}`: the arrow stretched to fit its label.
#let xarrow(sym: sym.arrow.r, it) = math.attach(math.stretch(sym, size: 100% + 1em), t: it)
