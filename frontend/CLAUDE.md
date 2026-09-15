# Frontend

One page. No build step for development, no bundler, no dependencies: `index.html` holds the markup, loads
`app.js` as an ES module over the feature modules in `js/`, and links nine stylesheets in cascade order.
`serve.py` serves the directory on port 3000 and proxies `/api/*` and `/{code}` to `cargo lambda watch` on
port 9000 exactly as CloudFront does in production, so the page calls the same paths in both places. A
change is checked by running `scripts/dev.sh` and opening `http://localhost:3000`, and by
`node --test "frontend/**/*.test.mjs"` for the pure rules.

The deployed artifact is one string: `lambdas/redirect/build.rs` inlines the nine stylesheets and the module
graph into `index.html` and writes the result to `OUT_DIR`, which `lib.rs` `include_str!`s and serves from
`GET /`. That is a property of the artifact, not of the source, and nothing in this directory is shaped by
it — see _The build_ below for the four things the inliner cannot express, which it refuses at compile time
rather than at runtime.

## What each module owns

A module owns one job, and nothing else imports its internals.

| Module | Owns |
| --- | --- |
| `js/api.js` | `request`. The only module that calls `fetch`, the only one that knows the `/api` prefix, and the only one that turns a response into `{ data, status: "OK" }` or `{ message, status: "ERROR" }`. |
| `js/dom.js` | `el`, `show`, `notify`, `element`, `escapeHTML`. The only module that looks an element up by id, and the only one that turns a string of markup into a node. |
| `js/format.js` | `codeList`, `isoDay`. The only module that parses a comma list or formats a date. |
| `js/link.js` | What a link is and what makes one invalid: `isCode`, `isHttpUrl`, `linkProblem`, `rulePayload`, the two ceilings and the refusal copy. No DOM, no `fetch`. |
| `js/store.js` | The remembered-links list: `savedLinks`, `rememberLink`, `forgetLink`. The only module that touches `localStorage`. It stores the management secret in the clear, on purpose — this app decided convenience outweighs confidentiality here, since the worst case of a leaked secret is a redirect rule changing, not a financial loss. No DOM, no `fetch`. |
| `js/rules.js` | The rule builder: the row markup, reading rows back into rules, the summary line, ordering, the add, move and remove wiring. `readRules`, `showRules` and `wireRules` are its surface; the list ids are its secret. |
| `js/stats.js` | The stats window: `statsPath`, the segment split, the per-segment rollup, the day ordering, and the two tables. |
| `js/create.js` | The create panel: submit, the result reveal, the two copy buttons, and remembering the link. `wireCreate`. |
| `js/manage.js` | The manage panel: load, edit, delete, which of the two responses to trust, the remembered-links picker, and the two internal views — `manage-list` (the picker and the load form) and `manage-edit` (its own `Link settings` / `Rules` / `Analytics` sub-tabs, one link at a time). `wireManage` and `showSavedLinks` are its surface; `showSavedLinks` is also how the module returns itself to the list view. |
| `app.js` | `boot`: the two tabs, refreshing the remembered-links list on switch to manage, then `wireCreate` and `wireManage`. |

`js/link.js`, `js/format.js` and `js/store.js` import nothing; `js/rules.js` and `js/stats.js` import only
`link.js`/`format.js` and `dom.js`; the two panels import everything below them; `app.js` imports only the
two panels and `dom.js`. A new module joins that order rather than reaching across it.

## Testing

A pure rule gets a table-driven test beside it — `link.test.mjs` next to `link.js` — importing the module
directly. **A function is exported so its test can import it only when it neither reads nor writes the
DOM**; a function that writes the page is never exported for a test, it is checked in the browser. That is
the same split the first JavaScript rule below asks for, arriving from the other direction: the half that
decides is the half that is tested.

Tests follow the repository root's rules: a `cases` array with a label per case, one behaviour per test with
the name saying which, and never a compound boolean. A test file is a `*.test.mjs` beside its module; the
build follows imports from `app.js`, so a test never reaches the page.

## The build

`lambdas/redirect/src/bundle.rs` is two hundred lines of `std`, compiled into `build.rs` and, under
`cfg(test)`, into the crate so its own table of refusals runs with `cargo test`. It replaces each
`<link rel="stylesheet" href="…">` with a `<style>` block in link order, and the
`<script type="module" src="app.js">` tag with one inline module script holding every module the import
graph reaches, dependencies first, with `import` and `export` lines stripped. `cargo:rerun-if-changed`
points at `frontend/`, so `cargo build` picks up an edit here without a separate step, and the page is never
committed as a generated file.

The inlined script is one scope, so four things a module graph allows are refused with a message naming the
module and the line:

- a top-level `const`, `let`, `function` or `class` name declared in two modules — `js/dom.js` and
  `js/stats.js` may not both declare `OTHER`;
- a `default` export, an `export { … }` list or an `export *`;
- an `import` that is not `import { … } from "./relative.js";` on one line — no default imports, no
  side-effect imports, no bare specifiers;
- a dynamic `import()` or `import.meta`.

The build also refuses a page over the brief's 50 KB budget. Top-level `await` is fine: the inlined script
is `type="module"`, strict and deferred, exactly like the source.

## JavaScript

- **A function either works out a value or writes to the DOM, never both.** `linkProblem` returns the
  sentence; `notify` writes it. `copyInto` returns `COPIED_MSG` or `COPY_FAILED_MSG` and the caller writes
  `copy-note`. Split any function that does both before extending it.
- **Async work returns a result object tagged with `status`.** `request` returns `{ data, status: "OK" }`
  or `{ message, status: "ERROR" }`, and each caller branches once on `result.status`. `submitLoad` reads
  two of them and decides which to trust. Do not assign to the same element inside three branches.
- **A constant's name carries its unit.** `DAY_MS`, `STATS_WINDOW_DAYS`. A number with no unit in its name
  is a bug waiting to be misread.
- **User-facing copy is a module constant above the functions, never a string literal inside control
  flow.** `INVALID_URL_MSG`, `SECRET_WARNING_MSG`, `BLIND_EDIT_MSG`. A sentence used by two messages becomes
  its own constant and is composed into both: `LIST_LIMIT_MSG` is one clause appearing in the countries and
  the regions refusal, and it is composed from `LIST_LIMIT` so the number cannot drift from the check.
- **One builder per message shape.** `dimension` picks the fragment that differs (`IN or GB` against `any`)
  and `ruleSummary` writes the surrounding line once; `ruleCountMessage` does the same for the count. Never
  copy a sentence into two branches to change three words.
- **Build markup with a template literal and `element` from `dom.js`**, not a run of `createElement` and
  `append`. `ruleRow` is the pattern to copy: destructure the parameter with defaults, give each generated
  fragment a name (`checkMarkup`), and pass every string that came from the API through `escapeHTML`. The
  escape map is a module constant, not an object literal rebuilt per character. `element` parses through a
  `<template>`, not a range: a detached range drops a `<tr>` or a `<td>` on the floor.
- **A helper stays private to the module that needs it until a second module imports it.** `ruleProblem`
  is private to `link.js` because only `linkProblem` calls it; `escapeHTML` is in `dom.js` because
  `rules.js` and `stats.js` both need it. Move, never copy.
- **Server refusals are rendered verbatim, never pre-empted in JavaScript.** The client checks only what
  keeps a pointless round trip off the table — the URL scheme, the code shape, the two ceilings. Everything
  else is the Lambda's sentence, shown as it arrived, so the two copies of a rule cannot drift.
- **Blank lines separate the phases of a function**: what it reads, what it decides, what it writes.
  `submitCreate` reads the form in the first paragraph, guards in the second, disables and calls in the
  third. A closing `show(…)`, `notify(…)` or `return` always gets its own paragraph.
- **Guard and return early.** `submitCreate` returns after reporting a problem rather than nesting the
  request in an `else`; the list click handler in `wireRules` returns when the click missed a button.
- **Hoist a repeated `el(id)` into a const.** `submitCreate` does `const submit = el("create-submit")` once
  instead of looking the button up to disable and re-enable it.
- **Predicates read as questions**: `isCode`, `isHttpUrl`. A bare adjective or verb reads as neither a
  question nor a command.
- **`const` arrow for a one-expression helper, `function` for anything longer or exported.** `el`, `show`,
  `codeList` and `byDateDescending` are arrows; `element`, `ruleRow` and `submitCreate` are declarations.
- **Alphabetical**: object literal keys, destructured parameters, named imports inside the braces, and the
  import statements themselves by module path.
- **No comments.** Name the thing instead. This is the same rule the repository root applies to Rust.

## CSS

- **One declaration per line, always.** Never collapse a rule because it only has two properties —
  `.block` in `02-forms.css` is formatted identically to a twenty-line rule.
- **Properties within a rule are alphabetical**, then nested selectors, then nested at-rules. `.cta` in
  `06-buttons.css` is the full shape: declarations, then `&:disabled`, `&:focus-visible` and `&:hover` in
  alphabetical order. Nested `@media` blocks go widest first so the narrower one overrides.
- **Within a file, element selectors come before class selectors**: `h1`, `h2`, `h3` before `.hint`, `.lede`,
  `.mono` in `01-typography.css`. A selector anchored by a class files among the classes, not the elements,
  because the class is what scopes it.
- **Component styles are wrapped in `@scope`.** `.rule-list`, `.site-header`, `.tabs`, `.result` and
  `.stats` each open an `@scope` block, and bare element selectors inside one (`table`, `td`, `th`) are
  scoped by it.
- **Never let a `span`, `div` or `label` stand in anonymously for part of a component — give it a class**
  (`.rule-head`, `.field-label`, `.bar-note`, `.copy-row`). Bare element selectors outside an `@scope`
  block are reserved for the document-wide reset in `04-layout.css` (`*`, `[hidden]`, `body`, `button`), the
  type scale in `01-typography.css` (`h1`, `h2`, `h3`) and the form defaults in `02-forms.css` (`fieldset`,
  `input`, `legend`).
- **A class may exist only to name a part.** `.rule-wide` carries nothing but a grid column at one
  breakpoint; the class is still required, so the markup says what the field is.
- **Style with classes, never with ids.** Ids are the JavaScript binding surface — `el("create-url")`,
  `el("stats-days")`, `el("delete-yes")` — and no stylesheet contains an id selector. Keep it that way.
- **Visibility is the `hidden` attribute, toggled through `show(id, visible)`.** `[hidden]` is forced to
  `display: none` in `04-layout.css`. Never hide something by setting `display` from JavaScript, and never
  write a `.is-hidden` class.
- **Colour comes from the `:root` tokens in `01-typography.css`.** A new literal hex is a missing token.
  `--text-on-fill` is named for a role rather than a shade: the ink `.cta` and `.danger` use on their solid
  backgrounds, which is not the `--bg` it would otherwise borrow.
