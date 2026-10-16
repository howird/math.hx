# math.hx preview test (markdown)

In markdown, `$...$` is inline and `$$...$$` is display math. `:math-preview`
reads the contents as **typst** (see `math-syntax!` for LaTeX).

## Inline

Can take up just 1 line $f = m a$.

Or two depending on the math: $x^2 + y^2 = r^2$ and $frac(a, b) = c/d$.

## Display Math

$$integral_0^infinity e^(-x^2) dif x &= sqrt(pi)/2$$

Code is skipped: `$not math$`

$$
"triu"(2) &= mat(1, 1; 0, 1) \
"triu"(3, 1) &= mat(0, 1, 1; 0, 0, 1; 0, 0, 0)
$$
