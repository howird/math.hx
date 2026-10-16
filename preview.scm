;; Rendered previews of math, drawn as kitty Unicode placeholders.
;;
;; The dylib renders the equation and uploads the image straight to the
;; terminal; all helix has to draw is a block of placeholder cells whose
;; foreground colour names the image. Needs kitty or ghostty.
;;
;; Two settings decide when and how. With auto-render on, the span the primary
;; cursor is in is shown for as long as the cursor stays in it; with it off,
;; only after `:math-preview`, until the cursor leaves that span. The image
;; goes below the math, either on blank lines reserved there with inlay hints
;; ('virtual) or as a popup painted over the text ('popup).

(require "math-hx.scm")
(require "helix/editor.scm")
(require "helix/misc.scm")
(require "helix/static.scm")
(require "helix/components.scm")
(require "helix/configuration.scm")
(require-builtin helix/core/text as text.)

(provide math-preview
         math-auto-render!
         math-auto-render-toggle
         math-auto-render-languages!
         math-display!
         math-background!
         math-syntax!
         math-font-size!
         math-next
         math-prev)

;; Rendered font size in pixels, inline and display math alike; 0 matches the
;; terminal's text.
(define *preview-px-per-em* 0)
(define *hover-name* "math-hx-hover")
;; Wait this long after an edit before re-rendering the hovered span.
(define *debounce-ms* 150)

;; Languages where auto-render runs. `:math-preview` and `]m`/`[m` work in any
;; language with math spans.
(define *auto-render-languages* '("markdown"))
;; 'on or 'off.
(define *auto-render* 'on)
;; 'virtual or 'popup.
(define *display* 'virtual)
;; Behind the image: 'popup (ui.popup) or 'background (ui.background).
(define *background* 'popup)
;; How `$...$` in markdown is read: 'typst, 'latex or 'auto.
(define *syntax* 'typst)

;; Settings are symbols from init.scm, but strings when run as typed commands
;; (`:math-syntax! auto`).
(define (set-choice! name value choices set)
  (let ([value (if (string? value) (string->symbol value) value)])
    (if (member value choices)
        (begin
          (set value)
          (schedule-sync!))
        (set-error! (string-append name ": expected one of "
                                   (apply string-append
                                          (map (lambda (c) (string-append (symbol->string c) " ")) choices)))))))

;;@doc
;; Set the languages auto-render runs in, e.g. `(math-auto-render-languages! '("markdown" "typst"))`
;; or `:math-auto-render-languages! markdown typst`.
(define (math-auto-render-languages! . languages)
  (set! *auto-render-languages*
        (if (and (= (length languages) 1) (list? (car languages))) (car languages) languages))
  (schedule-sync!))

;;@doc
;; Set auto-render: 'on (show the math the cursor is in) or 'off (only after `:math-preview`).
(define (math-auto-render! value)
  (set-choice! "math-auto-render!" value '(on off) (lambda (value) (set! *auto-render* value))))

;;@doc
;; Set how previews are drawn: 'virtual (in blank lines reserved below the
;; math) or 'popup (over the text below it).
(define (math-display! value)
  (set-choice! "math-display!" value '(virtual popup) (lambda (value) (set! *display* value))))

;;@doc
;; Set what's behind previews: 'popup (the theme's `ui.popup`) or 'background
;; (`ui.background`, blending in with the text).
(define (math-background! value)
  (set-choice! "math-background!" value '(popup background) (lambda (value) (set! *background* value))))

;;@doc
;; Set how `$...$` in markdown is read: 'typst (default), 'latex, or 'auto,
;; which reads spans with backslash commands like `\frac` as LaTeX and the
;; rest as typst.
(define (math-syntax! syntax)
  (set-choice! "math-syntax!" syntax '(typst latex auto) (lambda (syntax) (set! *syntax* syntax))))

;;@doc
;; Set the font size of previews in pixels, for inline and display math alike,
;; e.g. `(math-font-size! 24)` or `:math-font-size! 24`. 'auto (or 0, the
;; default) matches the terminal's text.
(define (math-font-size! size)
  (let* ([size (if (string? size) (or (string->number size) (string->symbol size)) size)]
         [px (cond
               [(equal? size 'auto) 0]
               [(and (number? size) (>= size 0)) (inexact->exact (round size))]
               [else #f])])
    (if px
        (begin
          (set! *preview-px-per-em* px)
          (schedule-sync!))
        (set-error! "math-font-size!: expected a size in pixels, or auto"))))

;;@doc
;; Toggle auto-render of the math under the cursor.
(define (math-auto-render-toggle)
  (math-auto-render! (if (equal? *auto-render* 'on) 'off 'on)))

;; ---------------------------------------------------------------------------
;; Finding and rendering spans.

;; Each span is (start end body block?), in char offsets, `end` exclusive.
(define (parse-math-spans doc-id language)
  (let ([text (text.rope->string (editor->text doc-id))])
    (cond
      [(equal? language "typst") (typst-math-spans text)]
      [(equal? language "markdown") (markdown-math-spans text)]
      [else '()])))

;; Each document's spans, as (doc-id language spans), so moving the cursor
;; doesn't parse the whole document again; dropped when it changes, or once
;; it's closed.
(define *span-cache* '())

(define (forget-spans! doc-id)
  (set! *span-cache*
        (filter (lambda (entry)
                  (and (not (equal? (car entry) doc-id)) (editor-doc-exists? (car entry))))
                *span-cache*)))

(define (math-spans doc-id)
  (let* ([language (editor-document->language doc-id)]
         [cached (let loop ([entries *span-cache*])
                   (cond
                     [(null? entries) #f]
                     [(equal? (car (car entries)) doc-id) (car entries)]
                     [else (loop (cdr entries))]))])
    (if (and cached (equal? (list-ref cached 1) language))
        (list-ref cached 2)
        (let ([spans (parse-math-spans doc-id language)])
          (forget-spans! doc-id)
          (set! *span-cache* (cons (list doc-id language spans) *span-cache*))
          spans))))

(define (span-start span) (list-ref span 0))
(define (span-end span) (list-ref span 1))
(define (span-body span) (list-ref span 2))
(define (span-block? span) (list-ref span 3))

;; The span containing `pos`, delimiters included.
(define (span-at spans pos)
  (let loop ([spans spans])
    (cond
      [(null? spans) #f]
      [(and (<= (span-start (car spans)) pos) (< pos (span-end (car spans)))) (car spans)]
      [else (loop (cdr spans))])))

(define (hex2 n)
  (let ([digits "0123456789abcdef"])
    (string (string-ref digits (quotient n 16)) (string-ref digits (remainder n 16)))))

;; The theme's text colour as #rrggbb, or white if it isn't an rgb colour.
(define (text-color)
  (let* ([fg (style->fg (theme-scope-ref "ui.text"))]
         [r (and fg (Color-red fg))])
    (if r
        (string-append "#" (hex2 r) (hex2 (Color-green fg)) (hex2 (Color-blue fg)))
        "#ffffff")))

;; Typst buffers are always typst; markdown follows `*syntax*`.
(define (syntax-for doc-id)
  (if (equal? (editor-document->language doc-id) "markdown") (symbol->string *syntax*) "typst"))

;; Everything a render of `span` depends on; it's rendered again when this
;; changes. The epoch changes when the dylib deletes uploaded images.
(define (render-key doc-id span)
  (list (span-body span)
        (span-block? span)
        (syntax-for doc-id)
        (text-color)
        *preview-px-per-em*
        (typst-math-epoch)))

;; Render `span` and upload it, returning (id cols rows) or an error string.
(define (render-span doc-id span)
  (typst-math-render (span-body span)
                     (span-block? span)
                     (text-color)
                     *preview-px-per-em*
                     (syntax-for doc-id)))

;; Draw rows [first-row, last-row) of image `id`, `cols` cells of it from the
;; left, with its top-left corner at x, y.
(define (draw-image! buf x y id cols first-row last-row background)
  (let* ([rgb (typst-math-id-color id)]
         [cell-style (style-fg background (Color/rgb (list-ref rgb 0) (list-ref rgb 1) (list-ref rgb 2)))])
    (let loop ([row first-row])
      (when (< row last-row)
        (frame-set-string! buf x (+ y row) (typst-math-placeholder id row cols) cell-style)
        (loop (+ row 1))))))

;; Below the cursor, or above it when there's no room below.
(define (below-cursor rect rows)
  (let ([cursor (car (current-cursor))])
    (if cursor
        (let ([below (+ (position-row cursor) 1)])
          (cons (position-col cursor)
                (if (<= (+ below rows) (area-height rect))
                    below
                    (max 0 (- (position-row cursor) rows)))))
        (cons 0 0))))

;; ---------------------------------------------------------------------------
;; Hover: the one preview shown, of the span the primary cursor is in.
;;
;; It's shown while auto-render covers the span, or after `:math-preview` was
;; run in it. A component paints the image; it has no event handler, so keys
;; go straight through to the editor. The image goes under the span's last
;; line: from its first body column for inline math, at the opening
;; delimiter's column for display math. With 'popup it's painted over the
;; text there; with 'virtual, one inlay hint first reserves blank rows under
;; that line, and the image goes in them.
;;
;; Helix only says where the primary cursor is on screen, so the image is
;; placed from there: the span is so many lines and columns from the cursor.
;; That only holds without soft-wrap; with it, the image goes below the
;; cursor instead (over the text) and no space is reserved. Virtual lines
;; between the cursor and the span's last line (inline diagnostics) and
;; inlay hints before the span on its line still throw the image off.

;; The hovered span: (view-id doc-id start), or #f.
(define *hover-key* #f)
;; The span `:math-preview` was run in, as *hover-key*, until the cursor leaves it.
(define *manual-key* #f)
(define *hover-span* #f)
;; The document's text when *hover-span* was read.
(define *hover-text* #f)
;; The width of *hover-span*'s delimiters, from when the parser last found it.
(define *hover-delim* #f)
;; The span's last good render, (id cols rows); kept while edits don't compile.
(define *hover-image* #f)
;; The `render-key` *hover-image* was rendered with, or that last failed to.
(define *hover-rendered-key* #f)
;; The last render error shown for the hovered span, so it isn't repeated.
(define *hover-error* #f)
;; The reserved rows: (view-id doc-id hint-id rows), or #f.
(define *hover-hint* #f)
(define *hover-soft-wrap* #f)
;; Set by an edit, cleared once *debounce-ms* pass without another.
(define *edit-pending* #f)
(define *hover-pushed* #f)

(define (auto-render? doc-id)
  (let ([language (editor-document->language doc-id)])
    (and (equal? *auto-render* 'on) language (member language *auto-render-languages*) #t)))

;; Whether lines wrap in `doc-id`: its language's setting, else the editor's.
(define (soft-wrap? doc-id)
  (with-handler
   (lambda (_) #f)
   (let* ([language (editor-document->language doc-id)]
          [config (and language (get-language-config language))]
          [lang (and (hash? config) (hash-contains? config 'soft-wrap) (hash-ref config 'soft-wrap))]
          [lang-enable (and (hash? lang) (hash-contains? lang 'enable) (hash-ref lang 'enable))])
     ;; Unset language settings come through as void.
     (if (and (hash? lang) (boolean? lang-enable))
         lang-enable
         (equal? (get-config-option-value "soft-wrap.enable") #t)))))

(define (line-of text pos)
  (text.rope-char->line text pos))

(define (column-of text pos)
  (- pos (text.rope-line->char text (line-of text pos))))

;; The screen column of `pos` from the start of its line, with tabs and wide
;; characters taking up the cells helix gives them.
(define (visual-column text pos tab-width)
  (let ([line-start (text.rope-line->char text (line-of text pos))])
    (display-width (text.rope->string (text.rope->slice text line-start pos)) tab-width)))

;; `doc-id`'s tab width: its language's setting, else helix's default of 4.
(define (tab-width doc-id)
  (with-handler
   (lambda (_) 4)
   (let* ([language (editor-document->language doc-id)]
          [config (and language (get-language-config language))]
          [indent (and (hash? config) (hash-contains? config 'indent) (hash-ref config 'indent))]
          [width (and (hash? indent) (hash-contains? indent 'tab-width) (hash-ref indent 'tab-width))])
     ;; Config numbers come through as floats.
     (if (and (number? width) (>= width 1)) (inexact->exact (round width)) 4))))

;; Index of the line break ending the line that holds `pos`.
(define (line-end text pos)
  (let ([next (+ (line-of text pos) 1)])
    (if (< next (text.rope-len-lines text))
        (- (text.rope-line->char text next) 1)
        (text.rope-len-chars text))))

(define (repeat-string s n)
  (let loop ([n n] [acc ""])
    (if (<= n 0) acc (loop (- n 1) (string-append acc s)))))

;; Reserved space that couldn't be dropped right away, as (view-id doc-id
;; hint-id). Helix can only remove hints from the document the focused view
;; shows, and moving focus to remove one would leave insert mode, so each
;; waits until its view is focused on its document again. Hint ids are line
;; numbers, so removing one from another document could hit a different hint.
(define *orphan-hints* '())

(define (remove-hint-id! hint-id)
  (remove-inlay-hint-by-id (list-ref hint-id 0) (list-ref hint-id 1)))

;; Whether the focused view is `view-id` showing `doc-id`.
(define (focused-on? view-id doc-id)
  (let ([focus (editor-focus)])
    (and (equal? view-id focus) (equal? doc-id (editor->doc-id focus)))))

(define (remove-hint!)
  (when *hover-hint*
    (let ([view-id (list-ref *hover-hint* 0)]
          [doc-id (list-ref *hover-hint* 1)]
          [hint-id (list-ref *hover-hint* 2)])
      (if (focused-on? view-id doc-id)
          (remove-hint-id! hint-id)
          (set! *orphan-hints*
                (cons (list view-id doc-id hint-id)
                      (filter (lambda (orphan)
                                (not (and (equal? (list-ref orphan 0) view-id)
                                          (equal? (list-ref orphan 1) doc-id))))
                              *orphan-hints*)))))
    (set! *hover-hint* #f)))

;; Remove the orphaned hint of the focused view's document, if any, and
;; forget those of documents that have since closed.
(define (remove-orphan-hint!)
  (set! *orphan-hints*
        (filter (lambda (orphan)
                  (cond
                    [(not (editor-doc-exists? (list-ref orphan 1))) #f]
                    [(focused-on? (list-ref orphan 0) (list-ref orphan 1))
                     (remove-hint-id! (list-ref orphan 2))
                     #f]
                    [else #t]))
                *orphan-hints*)))

;; Reserve `rows` blank lines under the last line of `span`.
(define (add-hint! doc-id span rows)
  (let* ([text (editor->text doc-id)]
         [hint-id (add-inlay-hint (line-end text (- (span-end span) 1)) (repeat-string "\n" rows))])
    (set! *hover-hint* (and hint-id (list (editor-focus) doc-id hint-id rows)))))

;; With `display-inlay-hints` off, helix drops every doc's inlay hints whenever
;; views are split, switched or closed. Removing a hint at an index past the
;; end of the doc is a no-op that only reports whether the view still has any.
;; A language server replacing the hints with its own can't be told apart.
(define (hint-wiped? doc-id)
  (not (remove-inlay-hint (+ 1 (text.rope-len-chars (editor->text doc-id))) "")))

(define (clear-hover!)
  (remove-hint!)
  (set! *hover-key* #f)
  (set! *hover-span* #f)
  (set! *hover-text* #f)
  (set! *hover-delim* #f)
  (set! *hover-image* #f)
  (set! *hover-rendered-key* #f)
  (set! *hover-error* #f))

;; The hovered span followed through the edits since the last sync, without
;; parsing the document: its end moves with the text's length, which holds
;; while the edits stay inside it, and its delimiters must be untouched. Good
;; enough while typing; once the edits pause, the span is checked properly.
(define (shifted-span view-id doc-id pos)
  (let ([span *hover-span*]
        [old *hover-text*]
        [delim *hover-delim*])
    (and span
         old
         delim
         *hover-key*
         (equal? *hover-key* (list view-id doc-id (span-start span)))
         (let* ([text (editor->text doc-id)]
                [len (text.rope-len-chars text)]
                [start (span-start span)]
                [old-end (span-end span)]
                [end (+ old-end (- len (text.rope-len-chars old)))]
                [slice (lambda (rope from to) (text.rope->string (text.rope->slice rope from to)))])
           (and (<= start pos)
                (< pos end)
                (<= (+ start delim) (- end delim))
                (<= end len)
                (equal? (slice old start (+ start delim)) (slice text start (+ start delim)))
                (equal? (slice old (- old-end delim) old-end) (slice text (- end delim) end))
                (list start
                      end
                      (slice text (+ start delim) (- end delim))
                      (span-block? span)))))))

;; The hovered span again, after edits that markdown no longer reads as math
;; (`$x $`, or a blank line inside `$$...$$`), so the image doesn't blink out
;; while typing. It's the same span if everything up to the end of its opening
;; delimiter and from the start of its closing one is untouched since the last
;; sync, and (in markdown) it reads as math again once that whitespace is
;; tidied up, so an escaped `\$` or a new code span still ends it.
(define (sticky-span view-id doc-id pos)
  (let ([span (shifted-span view-id doc-id pos)])
    (and span
         (let* ([old *hover-text*]
                [text (editor->text doc-id)]
                [start (span-start span)]
                [end (span-end span)]
                [delim *hover-delim*]
                [slice (lambda (rope from to) (text.rope->string (text.rope->slice rope from to)))])
           (and (equal? (slice old 0 (+ start delim)) (slice text 0 (+ start delim)))
                (equal? (slice old (- (span-end *hover-span*) delim) (text.rope-len-chars old))
                        (slice text (- end delim) (text.rope-len-chars text)))
                (or (not (equal? (editor-document->language doc-id) "markdown"))
                    (markdown-math-recovers (text.rope->string text) start end (span-block? span)))
                span)))))

;; Bring the hover in line with the primary cursor. While edits are coming
;; in, the document isn't parsed and nothing is rendered: the hovered span is
;; only followed through them, so the image moves with the text. Once they
;; pause for *debounce-ms*, the span is found and rendered properly.
(define (sync-hover!)
  (let* ([view-id (editor-focus)]
         [doc-id (editor->doc-id view-id)]
         [pos (cursor-position)]
         [auto? (auto-render? doc-id)]
         [looking? (or auto? *manual-key*)]
         [parsed (and looking? (not *edit-pending*) (span-at (math-spans doc-id) pos))]
         [found (or parsed
                    (and looking?
                         (if *edit-pending*
                             (shifted-span view-id doc-id pos)
                             (sticky-span view-id doc-id pos))))]
         [found-key (and found (list view-id doc-id (span-start found)))]
         [shown? (or auto? (equal? found-key *manual-key*))]
         [span (and shown? found)]
         [key (and shown? found-key)])
    (unless (and *edit-pending* (not span))
      (sync-hover-to! doc-id parsed span key))))

;; The rest of `sync-hover!`, once it has decided what to show.
(define (sync-hover-to! doc-id parsed span key)
  (remove-orphan-hint!)
  (unless (equal? key *manual-key*)
    (set! *manual-key* #f))
  (unless (equal? key *hover-key*)
    (clear-hover!)
    (set! *hover-key* key))
  (when span
    (set! *hover-span* span)
    (set! *hover-text* (editor->text doc-id))
    ;; `$$` around markdown display math; one `$` otherwise, typst's
    ;; `$ ... $` included.
    (when parsed
      (set! *hover-delim*
            (if (and (span-block? span) (equal? (editor-document->language doc-id) "markdown")) 2 1)))
    (set! *hover-soft-wrap* (soft-wrap? doc-id))
    (unless (or *edit-pending* (equal? (render-key doc-id span) *hover-rendered-key*))
      (let ([image (render-span doc-id span)])
        ;; After rendering, which may itself delete images and change the epoch.
        (set! *hover-rendered-key* (render-key doc-id span))
        (if (string? image)
            (unless (equal? image *hover-error*)
              (set! *hover-error* image)
              (set-error! image))
            (begin
              (set! *hover-error* #f)
              (set! *hover-image* image)))))
    (sync-hint! doc-id span)
    (ensure-hover-component!)))

;; Keep the reserved rows matching the image, or drop them under soft-wrap or
;; with 'popup.
(define (sync-hint! doc-id span)
  (let ([image *hover-image*])
    (cond
      [(or (not image) *hover-soft-wrap* (not (equal? *display* 'virtual))) (remove-hint!)]
      [(and *hover-hint*
            (= (list-ref *hover-hint* 3) (list-ref image 2))
            (not (hint-wiped? doc-id)))
       #f]
      [else
       (remove-hint!)
       (add-hint! doc-id span (list-ref image 2))])))

;; Where the image goes, as (x . y) in screen cells, or #f when it can't be
;; placed. Every line of a view starts at the same screen column without
;; soft-wrap, which the primary cursor gives away. Display math goes on the
;; line after it either way.
(define (hover-origin rect doc-id span cols rows)
  (let ([cursor (car (current-cursor))])
    (cond
      [(not cursor) #f]
      [*hover-soft-wrap* (below-cursor rect rows)]
      [(and (equal? *display* 'virtual) (not *hover-hint*)) #f]
      ;; An edit can redraw before the queued sync re-reads the span, which may
      ;; then run past the end of the shorter text.
      [(> (span-end span) (text.rope-len-chars (editor->text doc-id))) #f]
      [else
       (let* ([text (editor->text doc-id)]
              [tabs (tab-width doc-id)]
              [pos (cursor-position)]
              [last (- (span-end span) 1)]
              [line-x (- (position-col cursor) (visual-column text pos tabs))]
              [line-y (+ (position-row cursor) (- (line-of text last) (line-of text pos)))]
              ;; Display math lines up with its opening delimiter, inline math
              ;; with its first body character.
              [from (if (span-block? span) (span-start span) (+ (span-start span) 1))])
         (cons (+ line-x (visual-column text from tabs)) (+ line-y 1)))])))

(define (render-hover _state rect buf)
  (let ([image *hover-image*]
        [span *hover-span*])
    (when (and image span *hover-key* (equal? (list-ref *hover-key* 0) (editor-focus)))
      (let* ([doc-id (list-ref *hover-key* 1)]
             [id (list-ref image 0)]
             [cols (list-ref image 1)]
             [rows (list-ref image 2)]
             [area (editor-focused-buffer-area)]
             [origin (and area (hover-origin rect doc-id span cols rows))])
        (when origin
          (let* ([left (area-x area)]
                 [right (+ (area-x area) (area-width area))]
                 ;; Shifted left to fit in the view, and cut off on the right
                 ;; if it's wider than the view.
                 [x (max left (min (car origin) (- right cols)))]
                 [visible-cols (min cols (- right x))]
                 [y (cdr origin)]
                 ;; The view's last row is its statusline.
                 [top (area-y area)]
                 [bottom (- (+ (area-y area) (area-height area)) 1)]
                 [first-row (max 0 (- top y))]
                 [last-row (min rows (- bottom y))])
            (when (and (> visible-cols 0) (< first-row last-row))
              (draw-image! buf x y id visible-cols first-row last-row
                           (theme-scope-ref (if (equal? *background* 'popup) "ui.popup" "ui.background"))))))))))

(define (ensure-hover-component!)
  (unless *hover-pushed*
    (set! *hover-pushed* #t)
    (push-component! (new-component! *hover-name* #f render-hover (hash)))))

;; Deferred to a callback because hooks can fire after `:q` closes the last
;; view, when focus points at a view that no longer exists and
;; `editor->doc-id` panics helix. Helix checks for shutdown before running
;; queued callbacks, so a deferred sync never sees that state.
(define *sync-queued* #f)

(define (schedule-sync!)
  (unless *sync-queued*
    (set! *sync-queued* #t)
    (enqueue-thread-local-callback (lambda ()
                                     (set! *sync-queued* #f)
                                     (sync-hover!)))))

;; Counts edits; a delayed render only runs if no edit came after it.
(define *edit-generation* 0)

(define (schedule-debounced-render!)
  (set! *edit-pending* #t)
  (set! *edit-generation* (+ *edit-generation* 1))
  (let ([generation *edit-generation*])
    (enqueue-thread-local-callback-with-delay *debounce-ms*
                                              (lambda ()
                                                (when (= generation *edit-generation*)
                                                  (set! *edit-pending* #f)
                                                  (sync-hover!))))))

(register-hook! 'selection-did-change (lambda (_view-id) (schedule-sync!)))
(register-hook! 'post-command (lambda (_command) (schedule-sync!)))
(register-hook! 'document-opened (lambda (_doc-id) (schedule-sync!)))
(register-hook! 'document-changed
                (lambda (doc-id _old-text)
                  (forget-spans! doc-id)
                  (schedule-debounced-render!)
                  (schedule-sync!)))

;;@doc
;; Show the math under the cursor rendered as an image (kitty/ghostty), until
;; the cursor leaves it.
(define (math-preview)
  (let* ([view-id (editor-focus)]
         [doc-id (editor->doc-id view-id)]
         [span (span-at (math-spans doc-id) (cursor-position))])
    (cond
      [(not span) (set-error! "no math under the cursor")]
      ;; Already shown.
      [(auto-render? doc-id) #f]
      [else
       (set! *manual-key* (list view-id doc-id (span-start span)))
       (sync-hover!)])))

;; ---------------------------------------------------------------------------
;; `]m` / `[m`: select the next or previous span, like `]f` / `[f`.

;; In select mode the selection is extended to it, with the cursor on the
;; span's far side from the anchor, so the next `[m` starts before it.
(define (select-span! span)
  (set-current-selection-object!
   (range->selection
    (if (equal? (editor-mode) (string->editor-mode "select"))
        (let ([anchor (range-anchor (selection->primary-range (current-selection-object)))])
          (range anchor (if (< (span-start span) anchor) (span-start span) (span-end span))))
        (range (span-start span) (span-end span))))))

;;@doc
;; Select the next math span.
(define (math-next)
  (let* ([pos (cursor-position)]
         [spans (filter (lambda (span) (> (span-start span) pos))
                        (math-spans (editor->doc-id (editor-focus))))])
    (if (null? spans)
        (set-status! "no next math")
        (select-span! (car spans)))))

;;@doc
;; Select the previous math span.
(define (math-prev)
  (let* ([doc-id (editor->doc-id (editor-focus))]
         [spans (math-spans doc-id)]
         [pos (cursor-position)]
         [current (span-at spans pos)]
         [before (if current (span-start current) pos)]
         [earlier (filter (lambda (span) (< (span-start span) before)) spans)])
    (if (null? earlier)
        (set-status! "no previous math")
        (select-span! (list-ref earlier (- (length earlier) 1))))))
