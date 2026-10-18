import { createMemo } from "solid-js";
import { Marked } from "marked";
import markedKatex from "marked-katex-extension";
import DOMPurify from "dompurify";
import "katex/dist/katex.min.css";

// Markdown config shared by both parsers (GFM tables/strikethrough + soft line
// breaks), unchanged from the previous global `marked.setOptions` call.
const MARKDOWN_OPTIONS = { gfm: true, breaks: true };

// `throwOnError: false` is what makes LaTeX safe to render *while streaming*:
// the model's text arrives token by token, so a half-written expression (e.g.
// `$x^2` before the closing `$`, or `$\frac{1}{`) must not throw and blank the
// message. With it, KaTeX emits an inline `katex-error` span that keeps the raw
// source visible in red, and the rest of the message renders normally.
//
// `strict: "ignore"` silences KaTeX's console warnings for non-standard input
// (they would fire on every streamed keystroke) without changing the output.
//
// Note on single-dollar math: marked-katex-extension has no
// `singleDollarTextMath` switch (that is KaTeX's auto-render API). Its standard
// inline rule only opens `$…$` at the start of a line or after whitespace and
// only when the closing `$` is followed by whitespace/punctuation/end, which is
// exactly the safeguard against currency prose ("costs $5 and $10").
// `nonStandard` stays off (the default), i.e. the stricter rule is in use.
const md = new Marked(MARKDOWN_OPTIONS).use(
  markedKatex({ throwOnError: false, strict: "ignore" }),
);

// Safety net for the rare KaTeX failure `throwOnError` cannot intercept (fatal
// errors, not `ParseError`s). Dropping the extension degrades math back to the
// literal `$…$` text instead of throwing out of `createMemo`.
const mdPlain = new Marked(MARKDOWN_OPTIONS);

// DOMPurify's default profile already keeps KaTeX's HTML, SVG and MathML markup
// (including `style`/`encoding`/`xmlns` and the `mrow`/`mi`/`mo`/… nodes), but
// it strips the `<semantics>`/`<annotation>` wrappers — leaving the raw TeX
// source as loose text inside `<math>`, which screen readers would read twice.
// Re-allow just those two inert MathML container tags; no attributes, scripts,
// event handlers or URL schemes are opened up.
const SANITIZE_CONFIG = { ADD_TAGS: ["semantics", "annotation"] };

function renderMarkdown(text: string): string {
  let html: string;
  try {
    html = md.parse(text) as string;
  } catch (err) {
    console.warn("Markdown: LaTeX render failed, showing plain markdown", err);
    html = mdPlain.parse(text) as string;
  }
  return DOMPurify.sanitize(html, SANITIZE_CONFIG) as string;
}

// Render markdown text as sanitized HTML with Tailwind typography styling.
// `dark:prose-invert` remaps the prose palette for dark mode.
export default function Markdown(props: { text: string; class?: string }) {
  const html = createMemo(() => renderMarkdown(props.text ?? ""));
  return (
    <div
      class={`prose prose-sm prose-neutral max-w-none dark:prose-invert ${props.class ?? ""}`}
      innerHTML={html()}
    />
  );
}
