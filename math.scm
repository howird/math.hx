;; Typst previews of the math in a buffer, see preview.scm.

(require "math-hx.scm")
(require "preview.scm")
(require "helix/keymaps.scm")
(require "helix/configuration.scm")
(require-builtin helix/core/keymaps as helix.keymaps.)

(provide math-preview
         math-auto-render!
         math-auto-render-toggle
         math-auto-render-languages!
         math-display!
         math-background!
         math-syntax!
         math-font-size!
         math-next
         math-prev
         latex->typst)

;; Next/previous math, next to helix's other `]`/`[` motions. This is
;; `(keymap (global) ...)` spelled out, so the menu gets our labels: helix looks
;; commands' docs up by evaluating their bare names, which only works for
;; functions defined at the top level of init.scm.
(let ([bindings (get-keybindings)])
  (merge-keybindings bindings
                     (keymap (normal ("]" (m math-next)) ("[" (m math-prev)))
                             (select ("]" (m math-next)) ("[" (m math-prev)))))
  (helix.keymaps.keymap-update-documentation!
   bindings
   (hash "math-next" "Goto next math" "math-prev" "Goto previous math"))
  (keybindings bindings))
