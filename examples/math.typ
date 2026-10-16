= math.hx preview test

Put the cursor on any equation and run `:math-preview`.

== Inline (1 row)

A simple one: $x^2 + y^2 = r^2$, and Greek: $alpha + beta = gamma$.

A fraction inline: $frac(a, b) + sqrt(x)$.

== Display (taller)

$ integral_0^1 x^2 dif x = 1/3 $

$ sum_(i=1)^n i = (n(n+1))/2 $

$ mat(1, 2; 3, 4) mat(x; y) = mat(x + 2y; 3x + 4y) $

$ f(x) = cases(
  x & "if" x >= 0,
  -x & "otherwise",
) $

$ lim_(h -> 0) (f(x + h) - f(x))/h $

== Tall

$ mat(
  a_(1 1), a_(1 2), dots.c, a_(1 n);
  a_(2 1), a_(2 2), dots.c, a_(2 n);
  dots.v, dots.v, dots.down, dots.v;
  a_(m 1), a_(m 2), dots.c, a_(m n);
) $

== Errors

This one is broken and should show a typst error: $frac(a, b$

Not math, so: "no math under the cursor".
